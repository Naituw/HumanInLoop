# 需求：从 IM 创建可在电脑端接续的 Agent 任务

> 状态：已实现。
> 关联计划：`docs/plans/im-agent-task-launch.md`  
> 依赖：四渠道主动命令、通用单选卡、Agent 生命周期追踪、IM watch、daemon keepalive。  
> 当前平台：macOS（Terminal.app）与 Windows（Windows Terminal `wt.exe`）；Agent：Claude Code / Codex / Cursor / Grok / Pi。Linux 后置。
> Pi >=0.82.0 没有内置权限模式，选中后跳过权限卡并固定 `AgentDefault`；完整适配见 `docs/plans/pi-agent-integration.md`。

## 1. 背景与目标

同类工具通常在后台用 `claude -p`、`codex exec` 等非交互模式创建任务，再自行管理后台进程、
输出与会话生命周期。这与 AskHuman 的「IM 与电脑无缝切换」定位冲突：用户回到电脑后看不到一个
和亲手启动时相同、可直接继续输入的 Agent 终端，还会引入第二套生命周期管理。

本功能改为：用户从 IM 发 `/new`，选择工作目录、Agent 与权限策略，在渠道原生输入卡中填写任务；
提交后，电脑打开一个新的系统 Terminal 窗口，启动真实交互式 Agent TUI 并发送首条任务。用户回到
电脑时直接接续这个会话；启动后的状态、watch、插话、权限与结束继续使用现有能力。

目标：

1. `/new` 不带参数即可从四种 IM 创建任务；
2. 进入流程前先确认本机至少有一个可用 workspace 和一个可运行且已集成 AskHuman 的 Agent；
3. 冷启动时从五家本地会话索引推导最近工作目录，之后由 lifecycle 增量维护；
4. 用户每次都能明确选择或按设置决定 Agent 默认权限与 YOLO；
5. 任务只能从与该 flow 绑定的渠道原生输入组件提交，不劫持普通聊天消息；
6. task 不拼入 shell；Agent 获得真实 TTY、真实 cwd 与原生 TUI；
7. 启动成功后在来源 IM 自动 watch 新 session；
8. 本功能不成为 Agent manager，不负责 kill / resume / retry / poll lifecycle。

## 2. 静态调研结论

本轮只检查了本机 CLI `--help`、已有 hook 实测日志、本地会话文件字段、仓库代码与官方文档，
**没有启动任何 Agent 会话，也没有发送 prompt**。

### 2.1 五家 CLI 均支持交互 TUI + 初始 prompt

| Agent | 交互启动 | cwd | YOLO 覆盖 | 禁止使用的后台形态 |
|---|---|---|---|---|
| Claude Code | `claude <prompt>` | 进程 cwd | `--dangerously-skip-permissions` | `-p` / `--background` |
| Codex | `codex <prompt>` | 进程 cwd，也支持 `-C` | `--dangerously-bypass-approvals-and-sandbox` | `exec` |
| Cursor | `cursor-agent <prompt>` | 进程 cwd，也支持 `--workspace` | `--yolo` | `-p` |
| Grok | `grok <prompt>` | 进程 cwd，也支持 `--cwd` | `--always-approve` | `-p` / `--single` |
| Pi | `pi <prompt>` | 进程 cwd | 无内置覆盖参数 | 非交互 print/RPC 形态 |

实现统一先 `chdir(workspace)`，再以 argv 直接启动，不依赖四家不同的 cwd flag。Agent 默认权限模式
不加任何 override；YOLO 只添加上表固定 flag，不接受 IM 传任意 flags。

参考：

- [Claude Code CLI reference](https://docs.anthropic.com/en/docs/claude-code/cli-usage)
- [Cursor CLI overview](https://docs.cursor.com/en/cli/overview)
- [Grok CLI reference](https://docs.x.ai/build/cli/reference)
- Codex 结论以本机 `codex-cli 0.144.1 --help` 与 OpenAI Developer Docs MCP 的 CLI reference 为准。

### 2.2 最近 workspace 的冷启动来源

| Agent | 本地来源 | cwd / 最近时间 |
|---|---|---|
| Claude Code | `~/.claude/projects/*/*.jsonl` | 有界读取顶层 `cwd`；timestamp / mtime |
| Codex | `~/.codex/sessions/**/rollout-*.jsonl` | `session_meta.payload.cwd`；`started_at` / mtime |
| Cursor | `~/.cursor/projects/<encoded>/agent-transcripts/**` | transcript mtime；encoded path 只按现存目录唯一匹配恢复 |
| Grok | `~/.grok/sessions/*/*/summary.json` | `info.cwd`；`last_active_at` / `updated_at` |
| Pi | `~/.pi/agent/sessions/**/*.jsonl` | v3 session header 的 `cwd`；mtime；自定义 sessionDir 由 lifecycle 增量补入 |

Cursor 的 `~/.cursor/chats/*/*/meta.json` 没有 cwd，不能单独使用。当前静态样本中，Cursor
`agent-transcripts` session id 与 chats 有较高重合；路径恢复只接受文件系统中恰好一个现存解，
歧义、已删除或不可达路径全部跳过。

厂商索引只负责首次/按需补种。长期真值是 AskHuman 自己的 workspace index：每次 lifecycle 事件
已经带 `kind + session_id + cwd`，增量更新即可，不让厂商私有格式成为日常依赖。

### 2.3 四渠道都有不劫持普通消息的任务输入方式

| 渠道 | 任务输入 | 稳定路由身份 |
|---|---|---|
| 飞书 | card form：手动任务/TODO 单选 + 可见 input +「启动任务」 | message id + flow id |
| 钉钉 | 复用现有提问卡模板：手动任务/TODO 单选 + Input +「启动任务」 | outTrackId + flow id |
| Slack | 消息：手动任务/TODO 单选 + `plain_text_input` +「启动任务」 | message ts + block nonce |
| Telegram | 先选「输入新任务」/TODO；前者进入 `ForceReply`，后者进入可补充的确认卡 | picker id / `reply_to_message_id` |

飞书、钉钉、Slack 已有 Ask 卡文本输入实现可复用。Telegram Router 已能识别 reply-to；只有回复到
指定任务提示消息的文本才会启动，普通文本仍按既有 Ask / autochannel 规则处理。

### 2.4 默认 watch 可以在 lifecycle turn-start 后绑定

Claude / Cursor / Codex 既有 hook 实测日志中，turn-start 均有 `prompt + session_id + cwd`。Grok 使用
兼容 `UserPromptSubmit` hook，并提供 session / cwd。launch helper 另给子进程注入一次性 launch id；
reporter 只上报 launch id 与 task SHA-256，不上报 task 正文。

匹配优先级：精确 launch id → `kind + canonical cwd + task hash + claim time`。Codex shared app-server
可能不继承 TUI 环境，因此走第二条。匹配到新 session 后，来源渠道自动创建既有 watch 订阅。

## 3. 已确认决策（用户经 AskHuman 定案）

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 运行形态 | 新图形终端窗口 + 原生交互式 TUI；不使用 print / headless / background |
| D2 | 生命周期所有权 | 启动后交给既有 lifecycle；本功能不轮询、不停止、不 resume Agent |
| D3 | 平台范围 | **macOS 系统 Terminal.app + Windows Terminal `wt.exe`**；iTerm2 与 Linux 后置，Windows Server/RDS 多会话 broker 不在本期。 |
| D4 | 命令 | 只支持无参 `/new`；Slack 展示 `!new`；`/new <文本>` 回用法错误，不把文本当任务 |
| D5 | readiness | `/new` 开始先检查 feature/keepalive、Terminal、workspace、Agent；workspace 或 Agent 为 0 即停止 |
| D6 | Agent 可用判据 | 同时满足：login shell 可解析到真实 executable；lifecycle installed/current；AskHuman 集成 mode 为 CLI/MCP、Rule/skill 已安装且当前 CLI/MCP 通道产物可用 |
| D7 | 非门控项 | Rule/skill 正文过期、Subagent Guard 过期/缺失、PermissionRequest capability 与 Agent 登录认证不作为可选 Agent 的硬门控，只在集成更新提示 / readiness / 设置中提示 |
| D8 | 流程 | workspace → Agent →（按全局设置可选）权限 → 渠道原生任务输入 |
| D9 | 任务提交 | 飞书/钉钉/Slack 有 TODO 时在同一卡显示「直接输入新任务」和 TODO 单选、始终显示输入框；无 TODO 时保留纯输入卡；提交即启动 |
| D10 | 普通消息 | 不用「下一条普通文本」模式；只有绑定到 task input 身份的提交才消费为任务 |
| D11 | workspace 来源 | 冷启动解析四家 session 索引；之后由 lifecycle 增量更新 |
| D12 | workspace 权限 | 最近运行过且仍存在的目录直接成为 IM 候选，无需电脑端再次批准 |
| D13 | workspace 身份 | canonical absolute cwd；不提升到 git root，保留子目录 / worktree 语义 |
| D14 | workspace 展示 | 首卡只列最近 5 个并加「显示更多」；展开后按 IM 上限列其余；basename + 缩短父路径消歧；歧义或不存在不列 |
| D15 | 功能开关 | 默认关闭；设置「高级」开启时强制 daemon keepalive 并安装/刷新 daemon 登录项（2026-07-25 从「实验」Tab 转正，配置键不变） |
| D16 | 关闭语义 | 关闭功能不擅自恢复 daemon lifecycle，避免覆盖用户之后的 keepalive 选择 |
| D17 | 权限运行语义 | 仅两种：**Agent 默认行为**（无 flags）与 **YOLO**（固定 agent adapter flags） |
| D18 | 权限选择设置 | 全局三态：`每次询问`（默认）/ `总是 Agent 默认` / `总是 YOLO` |
| D19 | 每次询问流程 | Agent 选择后发 Default / YOLO 二选一卡；不预选；YOLO 用危险样式与 Agent 专属说明 |
| D20 | 来源活跃槽 | `/new` 把来源 IM 设为活跃槽，使新 Agent 后续 AskHuman 提问默认回到同一渠道 |
| D21 | 默认 watch | 成功启动后默认 watch，**只在发起 `/new` 的来源渠道**创建订阅 |
| D22 | watch 失败 | 60 秒内未匹配 lifecycle session：Agent 启动仍算成功，来源 IM 明确告警未能自动 watch |
| D23 | 启动安全 | task 不进入 shell；Terminal 命令只含 AskHuman 绝对路径 + 一次性 token |
| D24 | 一次性任务 | launch dir 0700、record 0600、短 TTL、原子单次 claim；旧卡 / 重复 callback 不能重复启动 |
| D25 | 终端留存 | Agent 退出后保留 shell / Terminal 窗口，由用户决定何时关闭 |
| D26 | 渠道范围 | 飞书 / 钉钉 / Telegram / Slack 语义一致，各用原生任务输入载体 |
| D27 | 真实 Agent 验收 | 任何启动真实 Agent、发送 prompt 或可能计费的测试，必须先经 AskHuman 明确批准 |
| D28 | Agent 集成 Tab 排序 | 「自动集成」完整区域放在前，「手动集成」提示词 / MCP 示例整体移到自动集成之后 |
| D29 | TODO 范围与展示 | 只读取所选 workspace 所属 git-root project 的待办；沿用上限 10 条、溢出提示和 whats-next 的「执行待办：」/`Run todo: ` 前缀；飞书/钉钉以琥珀色 `【TODO】` 代替文字前缀；自动待办也显示并标 `⚡` |
| D30 | TODO 与补充输入 | 默认选择「直接输入新任务」；最多选择一个 TODO；选择 TODO 后，最终任务为 TODO 原文 + 空行 + 可选补充输入 |
| D31 | TODO 出队 | 只有 Terminal 已成功打开才 best-effort `take` 所选 TODO 并写执行历史；启动失败保留；并发删除不阻止用卡片快照启动 |
| D32 | Telegram 交互 | 先发「输入新任务」/TODO 来源选择卡；手动输入沿用 ForceReply；TODO 进入带「启动任务」的确认卡，可直接启动，也可回复该卡补充后启动 |

## 4. 用户流程

### 4.1 正常流程（权限设置 = 每次询问）

```text
/new
  → readiness：workspace ≥1、可运行 Agent ≥1、Terminal 可用
  → 工作目录卡：HumanInLoop · ~/Developer（Codex，2 小时前）
  → Agent 卡：Codex / Claude Code / Cursor / Grok（只列 ready）
  → 权限卡：Agent 默认行为 / YOLO
  → 任务输入卡：
       Workspace: HumanInLoop · ~/Developer
       Agent: Codex
       权限模式: YOLO
       (●) 直接输入新任务
       ( ) 执行待办：⚡ 修复登录后弹窗失效
       ( ) 执行待办：补充安装回归测试
       [多行任务输入或对所选 TODO 的可选补充]
       [启动任务]
  → 新 Terminal 窗口启动 Codex TUI 并执行任务
  → IM 回执「终端已启动，正在等待 Agent 注册」
  → turn-start 匹配 session 后，在本渠道出现 watch 卡
```

当权限设置为「总是 Agent 默认」或「总是 YOLO」时跳过权限卡，但任务输入卡必须显示最终权限。
TODO 来自所选 workspace 的 git-root project，最多显示 10 条；选项沿用 whats-next 的本地化前缀，
飞书/钉钉用琥珀色 `【TODO】` 标记代替文字前缀。没有 TODO 时不显示单选项，仍使用原来的
纯输入卡。选择「直接输入新任务」时输入不能为空；选择 TODO 时输入可空，若有内容则与 TODO 原文以
一个空行拼接。组合后的任务仍受 3000 字符上限约束。

Telegram 在最后一步先发任务来源选择卡。选择「输入新任务」后，发送带 Workspace / Agent / 权限摘要
的 ForceReply，正文明确说明「回复本消息将立即启动」；只有 reply-to 匹配时才消费，旁边提供 inline
「取消」。选择 TODO 后改发确认卡：可以直接点击「启动任务」，也可以回复该确认卡添加补充文字后启动。

### 4.2 readiness gate

`/new` 先并行检查：

1. `agentTasks.enabled`；keepalive + daemon login item 状态；
2. macOS GUI session 与 Terminal.app，或 Windows 单交互桌面会话与 `wt.exe`；
3. workspace index：过滤不存在路径；为空或扫描过期时执行一次有界五家冷扫描；
4. 五家 Agent：固定命令在用户 login shell 中解析为 executable；lifecycle enabled 且
   installed/current；
   `agent_mode` 为 CLI/MCP、Rule/skill 已安装，且 CLI timeout Hook / MCP config 通道产物已安装并 current。
   Rule/skill 正文与 Subagent Guard 过期只显示集成更新提示，不阻止选择 Agent。

结果：

- workspace 与 ready Agent 均非空：进入选择；部分 Agent 不可用时只列 ready，并在卡片尾部给短原因；
- workspace = 0：停止，提示先运行一次 Agent 或在电脑设置手动添加目录；
- ready Agent = 0：停止，按五家列 binary missing / lifecycle off/outdated / integration off/outdated；
- Terminal / keepalive 不可用：停止并给设置修复入口；
- 不通过启动 Agent 来测试认证；首次启动若需要登录，登录界面留在 Terminal 中。

probe 使用固定命令名，不接受 IM 输入；Unix 在用户 login shell 里执行有界 `command -v`，Windows
使用系统 PATH 与已知 npm native package 路径解析，只接受实际可执行
文件，结果短时缓存。helper 运行于 Terminal login shell，使用同一 PATH 语义。

### 4.3 flow 并发与过期

- 每张 workspace / Agent / permission / task-input 卡都绑定独立 flow id，可并行创建多个任务；
- 每渠道活动 flow 设软上限，flow TTL 30 分钟；过期卡点击 / 回复不启动；
- 选择后前一张卡就地定格，避免重复点击改变已进入下一阶段的 flow；
- 新 `/new` 不自动取消旧 flow；未提交的 task input 按 TTL 自行过期；Telegram 另提供 inline「取消」。
- 任务卡保存 TODO id 与完整文本快照；TODO 被另一处先删除时仍可用快照启动，成功后 `take` 为幂等
  best-effort，不影响本次 launch。

## 5. 最近 workspace 模型

持久化：`~/.askhuman/agent-workspaces.json`。

```text
WorkspaceRecord {
  key: canonical absolute cwd,
  display_path,
  last_seen_at,
  agent_kinds[],
  last_agent_kind,
  pinned,
  hidden,
  source: lifecycle | claude | codex | cursor | grok | manual
}
```

规则：

- 同一路径跨 Agent 合并；默认保留最近 50 条，IM 最多展示 20 条；
- pinned 优先，其余按 `last_seen_at`；
- hidden 不进 IM；lifecycle 更新不能解除用户隐藏，只有电脑端手动操作可恢复；
- 每次展示、任务提交、helper claim 都复核仍是 directory；
- IM 默认展示 `basename · ~/parent`，同名时逐级增加父目录；完整路径只在电脑设置显示；
- Dev Instance 使用各自 `ASKHUMAN_HOME`，不与生产实例串用。

## 6. 权限策略

### 6.1 全局选择方式

```text
permissionPrompt = ask | agent-default | yolo
```

- `ask`（默认）：每次在 Agent 后显示权限二选一卡；
- `agent-default`：跳过卡片，adapter 不加权限 flag；
- `yolo`：跳过卡片，adapter 加固定 YOLO flag。

### 6.2 Agent 默认行为

完全尊重本机配置。它可能在 Terminal 等待本地审批；若 Claude / Codex 已配置 AskHuman 原生
PermissionRequest capability，则审批仍可按既有链路投放 IM。本功能不自动安装或修改 permission hook。

### 6.3 YOLO

| Agent | argv override | UI 风险说明 |
|---|---|---|
| Claude | `--dangerously-skip-permissions` | 跳过权限检查 |
| Codex | `--dangerously-bypass-approvals-and-sandbox` | 同时关闭审批与 sandbox |
| Cursor | `--yolo` | 自动允许命令（仍受 Cursor 明确 deny / 自身 sandbox 配置影响） |
| Grok | `--always-approve` | 自动批准工具执行（仍受 Grok sandbox 配置影响） |

任务输入卡必须再次以普通元数据行显示最终策略；不重复显示 YOLO 警告。任何 IM 文本都不能覆盖映射或增加其它
flags，不自动修改 Agent 的持久配置。

## 7. LaunchRecord 与 shell 安全

Terminal 只执行：

```text
<absolute AskHuman executable> __agent-launch <uuid-token>
```

```text
LaunchRecord {
  id,
  created_at,
  expires_at,
  source_channel,
  task,
  task_sha256,
  canonical_cwd,
  agent_kind,
  permission: agent-default | yolo,
  askhuman_exe,
  dev_instance_home?
}
```

- task 最多 3000 Unicode 字符，保留内部换行，拒绝 NUL；
- task / cwd / Agent / permission 都不拼入 AppleScript 或 shell command；
- helper 原子 claim、校验 owner/mode/TTL/cwd，读取后立即 unlink record；
- helper `chdir` 后以固定 adapter + 单一 prompt argv `exec` Agent；stdin/out/err 继承 TTY；
- prompt 以前导 `--` 或各 adapter 已验证的 positional boundary 防止前导 `-` 被当 flag；
- 不接受 raw path、raw executable、shell flags、command template；
- helper 设置 `ASKHUMAN_AGENT_TASK_LAUNCH_ID` 供 hook best-effort 精确关联。

## 8. 平台终端行为

- macOS 用 AppleScript 向 Terminal.app 创建**新 window**，不复用 tab，不 `activate` 抢焦点；
- Windows 每个任务使用独占窗口：`wt.exe -w askhuman-<uuid> new-tab --title
  "AskHuman Agent [<uuid>]" --suppressApplicationTitle --startingDirectory <cwd>
  <AskHuman.exe> __agent-launch <uuid>`；所有参数均为 direct argv，不把 task/cwd/Agent 拼进 PowerShell
  或 cmd 字符串；找不到 Windows Terminal 时返回清楚的安装提示；
- launch helper 在 tab 0 尚为活动页时枚举可见顶层窗口，要求标题精确等于固定标题、owner executable
  basename 精确为 `WindowsTerminal.exe` 且唯一命中，并把 UUID、HWND、owner PID 持久化；lifecycle hook
  再把同一 UUID 关联到 AgentRegistry；
- 聚焦先验证已登记 HWND 仍存在、PID 未变化且 owner 仍为 Windows Terminal。只有通过此前置条件，才可对
  已知存活的命名窗口调用 `focus-tab -t 0`；随后等待固定 tab 标题恢复，并通过 Win32
  restore/foreground 同一 HWND。验证失败、tab 0 未能激活或 Windows 拒绝前台切换均 fail closed；
- 使用平台默认终端环境执行固定 helper command，使 Agent 获得正常用户 PATH 与真实 TTY；
- Agent / helper 退出后 shell 保持，窗口保留终端历史；
- 设置页「测试 Terminal」只打开自检窗口，不构造 Agent、不发送 prompt；macOS 用于提前完成 Automation
  授权，Windows 用于验证 `wt.exe` 解析与启动；
- Automation 拒绝、Windows Terminal 缺失或 launch 失败时明确回 IM，绝不退化到后台 headless 任务。

## 9. 自动 watch

提交任务时登记 `PendingLaunchWatch`：launch id、来源 channel、kind、canonical cwd、task hash、claimed_at。

turn-start reporter 增可选字段：

- `launch_id`：从环境读取，仅接受 UUID；
- `prompt_sha256`：只对 turn-start 从 hook stdin 的 prompt 计算；不传原文。

daemon 先把 lifecycle event 应用到 AgentRegistry，再匹配 pending：

1. launch id 精确命中；
2. 否则匹配 kind + cwd + prompt hash + 60 秒窗口；多个完全相同的 Codex 并发 launch 按 claim 顺序
   与 hook 到达顺序一对一消费，不让一条 session 绑定两次。

命中后调用现有 watch 领域逻辑，在来源 channel 创建 subscription / card；它之后完全遵守既有 watch
更新、持久化、`autoEndWatch` 与 `/unwatch` 语义。

60 秒超时：删除 pending，回「Agent 已启动，但未检测到可关注会话」；不杀 Agent、不重启、不创建
幽灵 AgentRecord。watch 发送失败也只告警。

## 10. 配置与设置

新增顶层配置：

```json
{
  "agentTasks": {
    "enabled": false,
    "permissionPrompt": "ask"
  }
}
```

workspace 动态状态放 `agent-workspaces.json`，launch record 放短时私有 state 目录。

设置「高级 → 从 IM 创建 Agent 任务」（2026-07-25 前在「实验」Tab）包含：

- enabled；说明开启会强制 daemon keepalive / 登录自启；
- 权限选择方式三态；YOLO 持久选项有醒目风险提示；
- Terminal 可用状态 +「测试 Terminal」；
- workspace 主卡片只显示已保存数量与「管理工作目录」入口；入口打开独立面板，左上角为「完成」、右上角为 `+`，`+` 调用 macOS 系统目录选择器；列表每行使用统一 `…` 菜单执行 pin / hide / forget；
- 五家 Agent readiness：binary、lifecycle、CLI/MCP integration、可选/不可选原因；PermissionRequest
  只作辅助信息；
- 总体 readiness 摘要。

同一轮 UI 调整把 Agent 集成 Tab 的「自动集成」完整区域移到「手动集成」之前。顶部说明同步改为
先引导一键自动集成，再说明底部可手动复制提示词 / MCP 配置；不改变任何集成语义。

开启事务：`enabled=true` → `daemonLifecycle=keepalive` → reconcile daemon login item → ensure daemon。
任一步失败都显示错误。关闭只关功能，不自动改回 lifecycle。

## 11. 非目标

- 不做 headless Agent manager、日志采集、任务队列、kill / resume / retry；
- 不 attach 既有 Agent，不自动创建 git worktree；
- 不接受 IM raw path、任意 command / flags；
- 不自动安装 lifecycle / permission hook 或 Agent 集成产物；readiness 只检查并引导去设置修复；
- 首版不支持 iTerm2 / Ghostty / WezTerm / Kitty / 编辑器内置终端；
- 不支持 Linux；不支持 Windows Server/RDS 多交互会话 broker；
- 首版 task 仅文本，不把 IM 消息自身的附件映射到 Agent prompt；由其它入口创建的 Todo 附件随
  `/new` 送达 Agent 属后续独立需求，见 `docs/specs/todo-attachments.md`；
- 不保证自动 watch 一定成功；匹配失败按 D22 明确告警。

## 12. 风险与降级

| 风险 | 处理 |
|---|---|
| 厂商 session 格式变化 | scanner 分家、best-effort；失败不影响 lifecycle 增量与手动添加 |
| Cursor encoded path 歧义 | 只接受现存唯一解，绝不猜 |
| login shell rc 慢 / 输出噪音 | probe 有短 timeout，只接受绝对 executable，结果缓存 |
| lifecycle enabled 但 CLI 未登录 | 不通过启动 Agent 预检；认证 UI 留在新 Terminal，launch 回执注明 |
| task shell injection | task 不入 shell；私有 token + argv exec |
| YOLO 高风险 | 每次询问默认、不预选；权限选择卡与持久 YOLO 设置显示警告，任务输入卡只显示当前模式元数据 |
| 重复卡回调 / Telegram 重复回复 | flow stage CAS + record 原子 claim，最多启动一次 |
| 选择后目录被删 / symlink 换向 | submit 与 helper 双重复核 canonical identity |
| Terminal Automation 未授权 | 设置预检；远程失败不转 headless |
| Codex shared app-server 无 launch env | task hash + kind + cwd + claim time 关联 |
| lifecycle / watch 未出现 | 60 秒告警；Agent 仍运行，本功能不接管 |
| TODO 在任务卡存续期间被修改或删除 | 卡片保存 id + 原文快照；提交按快照启动，成功后的出队为 best-effort |
| TODO + 补充内容超过 task 上限 | 提交前校验组合后的 3000 字符上限；失败时保留 flow 与 TODO |

## 13. 验收标准

1. `/new` 只接受无参；带文本回用法，不创建 flow。
2. `/new` 在任何卡片前完成 readiness；无 workspace / 无 ready Agent 均返回分项诊断。
3. Agent 只有 binary executable + lifecycle installed/current + integration CLI/MCP current 同时成立才进入选项。
4. 冷扫描从五家既有 session 得到最近 cwd；不读取 prompt/transcript 正文；格式损坏不崩溃。
5. 四渠道完成 workspace → Agent → 权限（ask 模式）→ 原生 task input；普通消息不被 task flow 消费。
   飞书/钉钉/Slack 有 TODO 时同卡显示单选与输入，无 TODO 时不显示选项。
6. permissionPrompt 三态正确跳转；任务卡显示最终策略；四家 YOLO argv 映射准确。
7. 提交 task 立即创建一个新 Terminal window；task 特殊字符不执行 shell，cwd 正确，TUI 交互正常。
8. 重复 callback / reply 最多启动一次；取消、过期、目录变化不启动。
9. `/new` 把来源设为活跃槽；lifecycle 匹配后只在来源渠道自动 watch。
10. 60 秒不能匹配时明确告警但不把 launch 标为失败。
11. Agent 退出后 Terminal shell / window 保留；生命周期仍完全由现有 AgentRegistry 处理。
12. 自动测试、fixture、Terminal 自检均不启动 Agent；真实 Agent E2E 必须先获用户批准。
13. TODO 只来自所选 workspace 的 git-root project，最多显示 10 条，自动待办标 `⚡`；默认选择手动输入。
14. 选择 TODO 时可直接启动，或把补充内容以空行拼到 TODO 原文后；Terminal 打开失败不删除 TODO，
    打开成功才出队；并发删除仍按卡片快照启动且不会报错。
15. Telegram 先选择手动输入或 TODO；手动输入沿用 ForceReply，TODO 确认卡支持直接启动或回复补充后启动。

## 14. 反馈记录

- **2026-07-12**：macOS 首版；最近目录直接可选；冷启动必须解析四家 session；功能开启强制 keepalive。
- **2026-07-12**：用户要求 `/new` 只支持无参，task 放在最后的渠道原生输入载体，提交即启动。
- **2026-07-12**：Agent 必须同时满足 binary 已安装与 lifecycle 已开启；`/new` 最先做完整 readiness。
- **2026-07-12**：新任务默认在来源渠道 watch；60 秒匹配失败时 launch 成功但告警。
- **2026-07-12**：权限只暴露 Agent 默认 / YOLO；全局设置为每次询问（默认）/总是默认/总是 YOLO。
- **2026-07-12**：首版终端进一步收窄为系统 Terminal.app，iTerm2 后置。
- **2026-07-12**：Agent readiness 增加 AskHuman integration CLI/MCP current 门控；Agent 集成 Tab 改为
  自动集成在前、手动集成在后。
- **2026-07-12**：按实测反馈简化最终任务卡：提示文字 + 输入框 + 提交，不再显示启动/取消选项；
  Telegram 因 ForceReply 交互保留独立取消按钮。
- **2026-07-12**：钉钉任务输入复用既有提问卡模板；workspace 首卡只显示最近 5 个，点击
  「显示更多」后才展开其余候选。
- **2026-07-12**：钉钉任务卡输入框上方的提示 Markdown 使用 footnote 小字号 token，避免默认字号过大。
- **2026-07-12**：任务卡第一行先说明用途，随后 Agent/workspace/权限模式各占一行，末行说明将在
  Mac 的新 Terminal 窗口启动；任务卡不重复 YOLO 警告。
- **2026-07-12**：用途行使用稍大的粗体标题；元数据改为短横线 Markdown list，保证逐项换行。
- **2026-07-12**：Telegram 的 workspace/Agent/权限选择按钮直接显示对应名称，不使用重复的“选择”文案。
- **2026-07-12**：设置入口迁至「实验」；工作目录改为独立原生风格管理面板，通过系统 File Picker 添加，每行使用统一操作菜单。
- **2026-07-12**：Agent readiness 不保留设置页启动时快照；生命周期开关/更新后立即重算，每次进入「实验」时再刷新，以捕获 Agent 集成页或外部变更。
- **2026-07-12**：readiness 重算中的四家 login-shell binary 探测移至后台 blocking worker；进入「实验」时先完成切页，结果返回后再异步更新，不阻塞 GUI 调度线程。
- **2026-07-12**：readiness 中未满足的条件改为可点链接：`CLI ×` 打开该 Agent 官方安装文档，`Lifecycle ×` 跳到「高级」对应行，`Integration ×` 跳到「Agents」对应卡片；应用内目标滚动定位并短暂高亮。
- **2026-07-12**：用户禁止未经批准启动 Cursor 或其它真实 Agent 做实测，以免产生计费。
- **2026-07-12**：Rule/skill 正文或 Subagent Guard 需更新不再阻止 `/new` 选择 Agent；mode=None、
  Rule/skill 缺失、CLI/MCP 通道产物不可用仍严格门控。
- **2026-07-20**：任务输入新增所选 workspace 对应项目的 TODO 单选。飞书/钉钉/Slack 在同一卡保留
  始终可见输入框；默认手动输入，无 TODO 时不显示选项。
- **2026-07-20**：TODO 可附加补充文字，最终 prompt 为 TODO 原文、空行、补充文字；选项沿用
  whats-next 的「执行待办：」前缀和飞书/钉钉 `【TODO】` 富文本标记，包含自动 TODO 并标 `⚡`；
  只有 Terminal 成功打开才出队，并发删除时仍使用卡片快照。
- **2026-07-20**：Telegram 先选择手动输入或 TODO；手动输入沿用 ForceReply，TODO 使用可回复补充的
  二次确认卡。
- **2026-07-25**：设置入口从「实验」Tab 转正到「高级」Tab，紧随「IM 渠道按需发送」；不再受
  `experimental.enabled` 隐蔽开关控制，配置键与开关状态不变。readiness 的进入刷新随之改挂「高级」。
- **2026-08-19**：Lifecycle readiness 链接改到「Agents」Tab 对应 Agent 卡内的 lifecycle 行；
  readiness 继续分别显示 CLI / Lifecycle / Integration，显式关闭 lifecycle 会使该 Agent 不可选。
