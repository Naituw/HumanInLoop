# 需求：从当前 Agent 会话立即分叉新会话

> 状态：已实现并验收；Codex / Grok 真实活动源会话 E2E 通过（2026-08-09）。
> 关联能力：`docs/specs/im-agent-task-launch.md`、`docs/specs/gui-agent-task-launch.md`、
> `docs/specs/gui-agent-console.md`、`docs/specs/im-watch.md`、
> `docs/specs/agent-lifecycle-tracking.md`。
> 首版平台：macOS + Terminal.app。
> 首版 Agent：Claude Code / Codex / Grok；Cursor 暂不支持。

## 1. 背景与目标

AskHuman 已能从 IM 或 GUI 新建一个真实的交互式 Agent 任务，但新任务没有当前会话的上下文。
本需求增加 **Fork 会话**：源 Agent 保持原样继续运行，同时在同一个工作目录启动第二个原生会话；
新会话复制源会话在 fork 时已经持久化的上下文，并立即执行用户为新分支填写的首条指令。

远程操作是主场景：Agent 可能正在一个很长的回合中工作，尤其可能正阻塞在 AskHuman 等待人类回答。
用户不在电脑前时，应能从 IM 选择该 Agent、填写分支指令并启动 fork；新 Terminal 只是承载真实 TTY
和以后回到电脑时的可接续界面，不要求用户在电脑前输入。

目标：

1. 源 Agent 无需停止、切换或等到 `turn-end`，Fork 立即启动；
2. 只调用厂商原生 fork，不复制、拼接或改写私有 transcript / 数据库；
3. IM 支持 `/fork` 会话选择和精确编号；
4. GUI 支持 Agent 控制台与托盘会话菜单，并使用独立 Fork 窗口收集指令；
5. fork 后的新 session 继续复用 lifecycle、AskHuman 集成、权限接管、watch 和 Agent 控制台；
6. AskHuman 记录轻量父子关系，让用户能看出新会话从哪条会话分叉。

## 2. 语义边界

### 2.1 复制什么

Fork 复制的是**厂商原生会话 loader 在新进程启动时认可的已持久化会话上下文**。它通常包括历史
用户消息、助手消息、已经落盘的工具调用/结果，以及厂商已经生成的压缩摘要。

Fork 不复制：

- 当前仍在运行的 shell / 工具 / 后台进程 / subagent；
- 尚未完成的 AskHuman 工具进程或它未来收到的答案；
- prompt 输入框中尚未发送的文字；
- 当前进程内的临时 permission grants；
- Terminal UI 状态；
- 工作目录副本或 Git worktree。

源会话的在途 AskHuman 请求保持原样，仍绑定源 `session_id`，不会被 Fork 回答、取消或转移。新分支
取得新 `session_id`；若它之后提出相同问题，按现有「session key + fingerprint」规则是另一条请求，
不会与源会话错误合流。

### 2.2 Fork 时间点

Fork 不等待源回合结束。用户提交分支指令后立即打开 Terminal 并启动原生 fork 命令。分叉点是**新
Agent CLI 实际读取源 session 的时刻**，不是用户打开选择卡或 Fork 窗口的时刻。源 Agent 在这段
时间内仍可继续写 transcript，因此新分支可能包含提交前后刚刚持久化的额外前缀。

AskHuman 不自行裁剪半回合。Codex 原生 `thread/fork` 对进行中的 turn 使用 interruption marker；
Claude Code 与 Grok 由各自原生 resume/fork loader 处理。若某个受支持版本不能安全读取活动源会话，
应把该版本判为 `forkReady=false` 并给升级/不支持诊断，不能退化为等待 `turn-end`。

### 2.3 工作区

源和新分支使用同一个 canonical cwd。两边会看到并可能同时修改同一批文件，这是本需求明确接受的
语义。需要隔离时由用户让 Agent 自行创建 worktree；AskHuman 的 Fork 流程不创建、不提示也不管理
worktree。

## 3. 静态调研结论

调研日期为 2026-08-08；检查了本机 CLI `--help`、现有会话存储/生命周期实现和厂商官方文档，
没有启动真实 Agent 或发送可能计费的 prompt。

| Agent | 本机版本 | 原生入口 | 结论 |
|---|---:|---|---|
| Claude Code | 2.1.226 | `claude --resume <sid> --fork-session <prompt>` | 支持；新进程使用新 session ID，源会话不变 |
| Codex | 0.145.0 | `codex fork <sid> <prompt>` | 支持；CLI `fork` 为稳定命令，App Server 另有 `thread/fork` |
| Cursor Agent CLI | 2026.07.23-e383d2b | 无 | 不支持；只有 resume/continue 与创建空 chat |
| Grok | 1.0.0 | `grok --resume <sid> --fork-session <prompt>` | 支持；新 session 从会话副本继续 |

Cursor IDE 有手动 **Duplicate Chat**，但 AskHuman 的启动面是 `cursor-agent` CLI；IDE UI 自动化和私有
数据库复制都不是稳定接口，不能据此宣称支持。首版不做 transcript 注入式“近似 Fork”。

参考：

- [Claude Code Manage sessions](https://code.claude.com/docs/en/sessions#branch-a-session)
- [Codex CLI fork](https://learn.chatgpt.com/docs/developer-commands?surface=cli#cli-codex-fork)
- [Cursor Duplicate Chat](https://docs.cursor.com/en/agent/chat/duplicate)
- [Cursor CLI sessions](https://docs.cursor.com/en/cli/overview)
- [Grok sessions](https://docs.x.ai/build/features/sessions#forking)

活动源会话的真实并发 E2E 仍须在实现验收时逐家执行，并遵守本项目“启动真实 Agent / 发送 prompt
必须先经 AskHuman 明确批准”的既有约束。

## 4. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 核心行为 | 源 Agent 继续运行；提交指令后立即原生 Fork，不等待 `turn-end` |
| D2 | 复制边界 | 只复制原生已持久化会话上下文，不复制运行中进程、临时授权或未发送输入 |
| D3 | 工作区 | 与源会话共用 cwd；不创建 worktree |
| D4 | Agent 范围 | Claude Code / Codex / Grok；Cursor V1 不支持 |
| D5 | 能力判定 | 增加运行时 `forkReady` probe/cache；不能只按 AgentKind 写死 |
| D6 | 初始源状态 | Working + Idle 可选；Ended 不进入选择列表 |
| D7 | 选择后结束 | flow 已锁定的源会话随后 Ended，仍从其已持久化最终状态继续 Fork |
| D8 | 递归 | Fork 出来的 Working / Idle 会话可再次 Fork |
| D9 | 远程门控 | 复用 `agentTasks.enabled`；关闭时 `/fork` 不可启动远程 Fork |
| D10 | 无参命令 | `/fork` 永远显示会话选择卡，即使只有一个候选也不自动选 |
| D11 | 精确命令 | `/fork <编号>` 精确指定；不接受内联分支指令 |
| D12 | Slack | 与其它命令一致支持 `!fork` 备用前缀 |
| D13 | 远程入口 | 仅 `/fork` 命令；watch 卡和 AskHuman 问题卡都不加 Fork 按钮 |
| D14 | 权限 | 完全复用 `/new` 的全局 `permissionPrompt`，不推断源会话权限 |
| D15 | 分支指令 | 渠道绑定输入卡收集必填纯文本；不提供项目 TODO |
| D16 | 启动形态 | 新 Terminal + 原生交互 TUI + 初始 prompt；不使用 print/headless/background |
| D17 | 自动 watch | 只在发起 Fork 的来源 IM 自动关注新 session |
| D18 | 源提问 | 源会话在途 AskHuman 请求保持不动，Fork flow 不抢答、不取消 |
| D19 | GUI 入口 | Agent 控制台 + 托盘会话菜单 |
| D20 | GUI 窗口 | 独立、全局唯一 `fork-task` 窗口，形态类似“新建任务” |
| D21 | 控件复用 | 抽取/复用新建任务的权限、输入、校验、busy/error 控件和视觉样式 |
| D22 | GUI 门控 | 不受 `agentTasks.enabled` 约束；与 GUI 新建任务一样检查 macOS/Terminal/readiness |
| D23 | 分叉关系 | 匹配新 session 后持久化 `forkedFromSessionId`；控制台、托盘和 IM 列表前置展示父序号 |
| D24 | 匹配失败 | Terminal 已打开仍算 launch 成功；60 秒未匹配只告警，不停止 Agent |
| D25 | 安全 | task / sid / cwd 不进入 shell；继续使用私有一次性 LaunchRecord + argv exec |

## 5. 远程用户流程

### 5.1 `/fork`

```text
/fork
  → gate：agentTasks.enabled、Terminal、至少一个 fork-ready 会话
  → 始终显示会话选择卡：
       🟢 [3] Claude Code · HumanInLoop · 正在等待回答
          「权限弹窗记忆」
       ⚪ [7] Codex · HumanInLoop · 空闲
          「修复 watch 卡刷新」 · 从 #2 分叉
  → 选择源会话
  → permissionPrompt=ask 时：Agent 默认 / YOLO
  → 渠道绑定 Fork 输入卡：
       Fork Agent #3
       - Claude Code
       - HumanInLoop
       - 权限：Agent 默认
       - 原会话将继续运行
       [必填：新分支接下来做什么]
       [启动分支]
  → 新 Terminal 执行原生 fork + prompt
  → IM 回执「已请求分叉 #3，原会话继续运行；正在等待新会话注册」
  → lifecycle 匹配新 session #9
  → IM 回执「已分叉 #3 → #9」并在本渠道创建 watch 卡
```

`/fork <编号>` 跳过选择卡，并以稳定 `session_id` 锁定源会话；之后仍按全局权限策略进入权限/输入步骤。

### 5.2 候选会话

选择卡只列：

- `state in {working, idle}`；
- kind 为 Claude / Codex / Grok；
- cwd 存在；
- 对应 CLI binary、lifecycle、AskHuman integration 与 fork capability 当前可用。

不自动选择唯一候选。Cursor、旧版本或 readiness 不通过的 Agent 不做可点击项；卡片尾部按家族汇总
短原因，例如“Cursor Agent CLI 暂不支持 Fork”或“Grok 版本不含 `--fork-session`”。

选择时保存 source session ID、kind、canonical cwd、标题/编号展示快照。提交时重查 transcript/source、
cwd、binary、integration 与 fork capability，但不要求源记录仍是 Working/Idle；这样源进程在用户填写
指令期间正常退出也不会使 flow 无谓失效。

### 5.3 指令与并发

- 指令必填，trim 后不能为空；最多 3000 Unicode 字符；拒绝 NUL；
- 不接受 TODO、附件、raw flags、raw path 或 executable；
- 每个 Fork flow 有独立 flow ID，沿用 `/new` 的渠道绑定、stage CAS、30 分钟 TTL 和并发软上限；
- 重复卡 callback / Telegram reply 最多创建一个 LaunchRecord；
- 普通聊天消息不作为 Fork 指令；Telegram 只消费绑定 ForceReply；
- 新 `/fork` 不取消旧 Fork flow，也不改变源 AskHuman 请求；
- `/fork` 把来源 IM 设为活跃槽，保证新分支后续提问仍回到该渠道。

## 6. Watch 卡边界

四个 IM 渠道的活动 watch 卡维持既有“取消关注 / 立即刷新”两按钮，不增加 Fork 快捷入口，也不增加
新的 watch 回调或钉钉模板变量。远程 Fork 统一通过 `/fork` 或 `/fork <编号>` 发起；这样 Fork 不侵入
实时关注卡的既有交互，钉钉模板与默认模板 ID 也无需变更。

AskHuman 创建并成功匹配的子会话仍可在 watch 状态行轻量显示直接父会话，用于识别分支关系；该展示
不提供点击动作，不改变订阅、终态或按钮语义。

## 7. GUI 流程

### 7.1 入口

- Agent 控制台：会话详情头部的 Fork 按钮；
- 托盘：每条 Working/Idle 会话子菜单的 Fork 项；
- Cursor 显示禁用原因或不显示可执行项；Ended 不显示。

### 7.2 Fork 窗口

新增独立、全局唯一的 `fork-task` 窗口。它与 `new-task` 可同时存在，避免覆盖用户正在填写的新任务。
窗口展示固定源会话摘要：标题、Agent、workspace、Working/Idle 状态和可选父分支提示；表单只包含：

1. permissionPrompt=ask 时的 Agent 默认 / YOLO 单选；
2. 必填分支指令；
3. 启动分支按钮；
4. busy 与原位错误提示。

权限设置不是 `ask` 时只显示最终权限元数据。表单复用/抽取 `NewTaskForm` 的 permission 控件、输入、
校验、busy/error 和样式 token，不复制第二套近似实现；workspace/Agent/TODO 选择控件不出现。

重复从另一个会话打开 Fork：若窗口未提交，聚焦现有窗口、切换源并清空旧指令；busy 时不切换源，
入口给短提示。Terminal 成功打开后关闭窗口；失败保留源、权限和输入以便重试。

本地入口不受 `agentTasks.enabled` 约束，但仍要求 macOS、Terminal.app、源 session 与 adapter readiness。
成功启动不切换 IM 活跃槽；若 daemon 在运行，仍登记 pending fork 以便记录父子关系。

## 8. Adapter 与 LaunchRecord

### 8.1 数据模型

把现有 LaunchRecord 扩展为兼容枚举，不新建第二套私有启动协议：

```text
LaunchMode = New | Fork { source_session_id }

LaunchRecord {
  id,
  created_at,
  expires_at,
  source,
  task,
  task_sha256,
  payload_sha256,
  cwd,
  kind,
  permission,
  executable,
  askhuman_executable,
  launch_mode
}
```

反序列化缺失 `launch_mode` 的旧记录时按 `New`，保证应用换新期间兼容。`payload_sha256` 必须覆盖
launch mode 与 source session ID，防止一次性 record 被替换目标。source session ID 只接受各家当前
会话 ID 格式的非空值；adapter 不接受 IM/GUI 传入命令模板。

### 8.2 固定 argv

helper claim、校验 cwd/executable/TTL/hash 后 `chdir(cwd)`，再按固定 adapter `exec`：

```text
Claude: <claude> [fixed-yolo] --resume <sid> --fork-session <prompt>
Codex:  <codex> fork [fixed-yolo] <sid> <prompt>
Grok:   <grok> [fixed-yolo] --resume <sid> --fork-session <prompt>
```

实际参数顺序与 `--` positional boundary 必须用本机 parser 单测/无计费探针逐家验证，确保以 `-` 开头的
prompt 不会被解释为 flag。YOLO 仍只允许现有固定映射：

- Claude `--dangerously-skip-permissions`；
- Codex `--dangerously-bypass-approvals-and-sandbox`；
- Grok `--always-approve`。

Agent 默认不加 override；不尝试继承源进程内 permission grants。Terminal shell 命令仍只有：

```text
<absolute AskHuman executable> __agent-launch <uuid-token>
```

task、sid、cwd 和 flags 均不拼入 AppleScript/shell。

### 8.3 `forkReady`

在现有 binary/lifecycle/integration readiness 之外增加 fork capability：

- Claude：固定 executable 的 `--help` 明确包含 `--fork-session`；
- Codex：`codex fork --help` 成功且契约含 session positional；
- Grok：固定 executable 的 `--help` 明确包含 `--fork-session`；
- Cursor：V1 恒 false，诊断区分“CLI 不支持”，不是“未安装”。

probe 运行在 login shell/后台 blocking worker，设短 timeout；按 canonical executable + 版本或文件指纹
短时缓存。最终 helper claim 后仍须 fail-closed 复检必要条件，避免选择卡到提交之间 CLI 被替换。

## 9. 新 session 匹配与分叉关系

统一扩展现有 PendingLaunch，而不是另建一套轮询器：

```text
PendingLaunch {
  id,
  source_channel?,
  kind,
  cwd,
  task_sha256,
  created_at,
  mode: New | Fork { source_session_id }
}
```

remote flow 在 Terminal 打开前登记来源 channel；GUI flow 即使没有自动 watch，也登记无 channel 的
pending fork，用于父子关系。turn-start reporter 继续只上报 launch ID / prompt hash，不上报 prompt 正文。

匹配优先级沿用 `/new`：

1. launch ID 精确匹配；
2. Codex shared app-server 等拿不到 env 时，按 `kind + canonical cwd + prompt hash + 60 秒时间窗`；
3. 多条完全相同 launch 按 claim 顺序和 hook 到达顺序一对一消费。

匹配成功：

- 在新 AgentRecord 写 `forked_from_session_id`；
- remote flow 回复 `#源 → #新`，并只在来源 channel 建 watch；
- GUI/托盘下一帧自然出现新 session；
- 父 session 无任何状态变化。

`AgentRecord`/IPC/TS 增加可选 `forkedFromSessionId`，旧持久化记录兼容缺失。展示时优先解析父记录的
当前 daemon seq；父记录已淘汰则回退短 session ID/标题，不删除子记录关系。同标题父子会话必须在
可选择列表中直接区分：控制台子行和 IM 选择卡以“从 #父序号 分叉”开头；托盘保留 `[子序号]`
在最前，随后立即显示“从 #父序号 分叉”，再显示 Agent 与标题，避免长标题截断谱系。不要求重建
厂商历史中所有既有 fork，只记录 AskHuman 发起并可靠匹配到的 fork。

60 秒未匹配：移除 pending；remote 回“Agent 已启动，但未检测到分支会话，无法自动 watch/记录关系”；
local 仅记录日志并由控制台现有列表反映实际 lifecycle。均不 kill、不 retry、不把源会话改状态。

## 10. 权限与 AskHuman 行为

Fork 完全复用 `agentTasks.permissionPrompt = ask | agent-default | yolo`：

- `ask`：每次在目标选择后显示 Agent 默认 / YOLO；不预选；
- `agent-default`：跳过选择，无 flags；
- `yolo`：跳过选择，使用固定 adapter flag。

源会话的 permission mode、临时 allow 和 AskHuman permission memory 不作为推断依据。新 session ID 使
现有 session-scoped permission memory 自然隔离；若用户选择 Agent 默认，Claude/Codex 可继续走现有
PermissionRequest 远程接管，Grok 可能在 Terminal 等原生批准，这是用户选择 Agent 默认的既有语义。

Fork 源正处于 `waitingRequestId` 时：

- `/fork` 命令和本地 GUI 入口保持可用；
- 原提问卡仍可照常回答；
- 新分支初始 prompt 立即执行；
- 不把原问题答案或 pending request ID 复制到新 session；
- 不触发 duplicate-ask 跨 session 合流。

## 11. 失败与降级

| 失败点 | 行为 |
|---|---|
| `agentTasks.enabled=false`（remote） | 不创建 flow，提示去高级设置开启 |
| 无候选 | 分家显示 Cursor 不支持 / 版本缺 capability / readiness / cwd 原因 |
| 旧编号或不 eligible | 不进入权限/输入卡，给当前状态与原因 |
| flow 期间源进程结束 | 若 transcript/source 仍在则继续 Fork |
| source transcript 不存在或已损坏 | 提交失败，保留输入，不退化为 transcript 注入 |
| cwd 删除或 identity 改变 | 提交/helper fail-closed，保留 flow |
| Terminal Automation 拒绝 | 明确失败，不转 headless |
| native fork 启动后报错 | 错误留在 Terminal；60 秒未注册再从来源渠道告警 |
| lineage/watch 匹配失败 | Agent 继续运行；只缺关系和自动 watch |
| watch 卡 | V1 不提供 Fork 按钮；远程统一使用 `/fork`，无需渠道模板升级 |
| Cursor | 明确不支持；不复制数据库、不注入 transcript、不自动点 IDE |

## 12. 非目标

- Cursor CLI 的近似 Fork、Cursor IDE UI 自动化或私有 DB 修改；
- 从 Ended 历史列表主动 Fork；
- 复制工作区、创建 worktree、Git 分支或文件快照；
- 在 AskHuman 问题卡上增加 Fork 动作；
- 内联 `/fork <编号> <任务>`；
- Fork 输入卡中的 TODO、附件或任意 Agent flags；
- 复制在途工具、后台任务、subagent、未发送输入或临时授权；
- 以 headless/print 模式替代真实 TUI；
- 自动导入/重建用户在厂商 UI 中既有的 fork lineage。

## 13. 验收标准

1. `/fork` 在有且仅有一个候选时仍显示选择卡；`/fork <编号>` 精确直达；内联任务回用法错误。
2. 选择卡只列 fork-ready 的 Working/Idle Claude、Codex、Grok；Cursor 给明确不支持诊断。
3. 源 Agent 正 Working、Waiting AskHuman 或 Idle 时都能立即提交 Fork，不等待 `turn-end`。
4. 源 AskHuman 卡在 Fork 前后保持可答；Fork 不产生取消、回答或跨 session 合流。
5. permissionPrompt 三态与 `/new` 一致；分支输入必填、≤3000 字符且不提供 TODO。
6. 三家 adapter 使用原生 fork 创建新 session ID，源 transcript/session ID 不变并继续运行。
7. fork task 中的引号、换行、反引号、`$()`、前导 `-` 等均只作为 argv prompt，不执行 shell。
8. remote launch 在来源渠道自动 watch 新 session；其它渠道不自动订阅。
9. 成功匹配后 AgentRecord 持久化 `forkedFromSessionId`，回执显示 `#源 → #新`；控制台、托盘和
   IM 会话选择卡均以前置父序号区分继承标题的父子会话。
10. 新分支可再次 Fork，关系指向直接父 session。
11. 四渠道活动 watch 卡保持既有两按钮，不出现 Fork 按钮或新 callback；钉钉模板与默认 ID 不变。
12. GUI 控制台和托盘入口打开独立全局唯一 Fork 窗口；与 new-task 窗口可同时存在；控件复用。
13. flow 锁定后源会话退出但 transcript 仍在时，提交仍能成功 Fork。
14. 60 秒匹配失败只告警，不停止新 Agent、不影响源 Agent。
15. 自动测试不得启动真实 Agent；活动源真实 E2E 必须先经 AskHuman 明确批准，并至少覆盖源正在
    等待 AskHuman 时 Fork、新旧 session 同时存活、初始 prompt 立即执行。

## 14. 实现验收

- 自动化回归覆盖三家固定原生 argv、能力探测、一次性 LaunchRecord、父子匹配、IM flow、GUI/托盘
  入口、四渠道渲染以及 Watch 卡无 Fork 按钮的边界。
- **Codex 真实 E2E（2026-08-08）**：源会话停在 AskHuman 时立即 Fork；修复 Terminal.app 新 tab
  启动时的 job-control race 后，新 TUI 正常启动并执行首条指令，源/分支同时存活，子记录准确指向
  父 session。原生 Fork 继承相同标题，因此补充了控制台、托盘和 IM 列表的前置谱系标识。
- **Grok 真实 E2E（2026-08-09）**：Grok 1.0.0 源 `#18` 停在 AskHuman 时创建子 `#19`；两者
  同时存活，子记录准确指向源 session，分支仅凭继承上下文复述未出现在分支 prompt 中的秘密短语，
  并输出 `GROK_FORK_E2E_OK`。
- Claude Code 的原生契约、adapter 与自动化回归已覆盖；按用户批准的本轮范围未启动真实 Claude E2E。

## 15. 反馈记录

- **2026-08-08**：用户确认只复制会话上下文、共用工作区；工作区隔离由 Agent/用户自行处理。
- **2026-08-08**：核心场景是远程 Fork 正在运行且可能等待 AskHuman 的 Agent，明确禁止等待
  `turn-end`；Working + Idle 均可选，Ended 不主动提供。
- **2026-08-08**：V1 支持 Claude/Codex/Grok，Cursor CLI 不做伪 Fork；远程入口统一为 `/fork`，
  watch 卡和 AskHuman 问题卡均不扩展按钮。
- **2026-08-08**：用户明确收窄入口，要求四渠道 watch 卡保持原两按钮，并回滚钉钉模板资产改动。
- **2026-08-08**：`/fork` 总是选择会话、不自动选；不接受内联指令；Fork 输入只收必填指令，不含 TODO。
- **2026-08-08**：权限复用 `/new` 全局策略，remote 复用 `agentTasks.enabled`；GUI 为控制台 + 托盘，
  使用类似新建任务、但独立全局唯一的 Fork 窗口并复用控件。
- **2026-08-08**：AskHuman 记录并轻量展示父子关系；选择后源进程结束时仍允许从持久化状态 Fork。
- **2026-08-08**：真实 Codex Fork 暴露 Terminal.app 新 tab 的 TTY job-control race，改为先创建并
  等待 tab 就绪再注入 helper 命令；复测通过。
- **2026-08-09**：原生 Fork 继承标题属于正常行为；用户要求同标题父子会话在所有列表中可直接
  区分，并确认托盘格式为 `[子序号] 从 #父序号 分叉 · Agent — 标题`，把谱系放在长标题之前。
