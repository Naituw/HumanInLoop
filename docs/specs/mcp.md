# 需求：MCP 模式支持（Codex / Claude Code / Cursor / Grok）

> 状态：已实现，覆盖 Codex / Claude Code / Cursor / Grok。
> Pi Agent 明确不进入 MCP 能力面：Pi 集成只提供 None/CLI，并由受管 Extension 处理超时与生命周期；见 `docs/plans/pi-agent-integration.md`。
> 关联计划：`docs/plans/mcp.md`
> 影响面：新增 `AskHuman mcp` 子命令（STDIO MCP server）、MCP 工具与参考提示词、四家 MCP 配置集成、设置「Agent」Tab 三态模式、`agents`/`doctor` CLI 状态、i18n 与 `rmcp`依赖。上下文恢复另追加了向后兼容的 daemon IPC 内部消息，**不改** stdout 结果区块契约、退出码语义、四个 IM 渠道和弹窗交互。

> **实现期补充（2026-07）**：Grok 仅支持 None/MCP，产物为 interaction-protocol skill +
> `~/.grok/config.toml`，并写 per-tool `ask` 超时。MCP 客户端可能清空 Agent 环境变量，子 CLI 因此把
> caller pid 上送 Daemon；Daemon 进程树探测只按 pid 刷新**已有** lifecycle session，绝不新建会话。
> rmcp cancellation 会终止子 CLI，socket EOF 再取消 Daemon 请求。Codex 配置还把 `mcp__askhuman`
> 最小加入 Code Mode `direct_only_tool_namespaces`，确保 ask 在顶层阻塞；所有权记录防止卸载用户原有项。
> Codex CLI 0.147 的 Ctrl+C 实测可能在本地丢弃 tool call、只向精确 rollout turn 写
> `turn_aborted`，而不发送 MCP `notifications/cancelled`。AskHuman 因此仅在 metadata 同时提供精确
> `session_id` 与 `turn_id` 时，从调用开始的 rollout EOF 监听该 turn 的 abort 作为兼容保险；原生 MCP
> cancellation 仍是主路径，其他 Agent、其他 turn 和历史 abort 均不能取消当前调用。
>
> **实现期补充（2026-07-23）**：Codex 桌面版 Suggested prompts 使用
> `thread_source=system` 的内部 thread。AskHuman 对 Codex 每次 `tools/call._meta` 做前置检查，命中
> `x-codex-turn-metadata.thread_source == "system"` 时在任何子进程、daemon 或 todo 副作用前本地拒绝
> `ask`、`whats_next`、`show_last`、`todo_add`、`todo_update`，并向 `~/.askhuman/daemon.log` 追加一条结构化抑制
> 审计；其它来源与缺失 / 异常 metadata 保持 fail-open。
>
> **实现期补充（2026-07-24）**：ChatGPT.app 26.721 把 ambient/Suggested prompts 线程的
> `thread_source` 标签从 `system` 改名为 `ambient_suggestions`，导致仅匹配 `system` 的 guard
> fail-open。拦截集合改为 `{system, ambient_suggestions}`（与 Stop 确认 guard 共用），并且对**未命中
> 拦截**的 Codex 调用也写一条 `action="passed"` 放行审计（含观测到的 `thread_source` 原值），未来宿主
> 再改名时可直接从 daemon.log 读到新标签。
>
> **实现期补充（2026-07-22）**：上下文压缩恢复新增只读 `show_last`。Codex 从每调用
> `_meta.threadId` 绑定，Claude/Cursor 由 mode 托管的 PreToolUse Hook 注入 schema 隐藏、
> 30 秒一次性 token，Grok 在同一 MCP instance+项目+进程分区内只认领参数指纹
> 候选恰好一条的调用。无真实 session 时才回退当前 MCP instance+项目；详见
> `docs/plans/agent-context-compaction-retention.md`。
> 恢复输出采用纯 CLI 区块，只保留非空 Message/附件、问题与人类实际答案；未选候选项和
> recommended 标记仍保留在历史记录中，但不重新注入压缩后的模型上下文。

## 1. 背景与动机

参考提示词要求 AI 用 Shell 调 `AskHuman` 并把该次工具调用超时设到 24h。这条对 **Cursor / Claude Code** 可行（二者有 PreToolUse/Bash 超时 Hook 把 Shell 工具调用 timeout 抬到 24h），但对 **Codex 不可行**：Codex 没有 Shell 超时 Hook，CLI（Shell）调用超时短且无法延长，等待人类回应时会被强制取消。

调研结论（外部核实）：

- **Codex 的长超时只能靠 MCP**：Codex MCP 工具调用默认超时 `tool_timeout_sec = 60s`，但**可在 `~/.codex/config.toml` 的 `[mcp_servers.<name>]` 里调大**（如 `tool_timeout_sec = 86400`）。即「Codex 用 MCP 时超时可以很长」是指**可配置**，而非默认。
- **MCP 模式不需要超时 Hook**：超时改由 MCP 配置项（Codex `tool_timeout_sec`）控制；Cursor/Claude 的 MCP 工具调用不受 Shell 10 分钟硬上限约束（具体上限实现期复核）。
- **图片可直接返回**：MCP 协议支持 `ImageContent`(base64 + mimeType) 作为工具结果；Codex 近期版本（2025-10 起，PR #5600 等）、Claude 均能把 MCP 图片喂给模型。CLI 模式只能把图片落盘后回传路径让 AI 再读——**直返图片是 MCP 模式相对 CLI 的实质增益**。
- **Rust 官方 SDK `rmcp`**（0.16，`server` + `transport-io` + 宏）可低成本实现 STDIO server。
- **MCP（STDIO）拿不到 turn 级生命周期**：MCP server 由客户端在 **session 期间**拉起常驻，协议无 turn-start/turn-end 通知，server 只在「工具被调用」时知情。故 MCP **替代不了** lifecycle hook 的 turn 追踪。

因此本需求为四家 Agent 增加 **MCP 模式**，与现有 **CLI 模式**互斥；Grok 因 CLI harness 限制仅提供 MCP。

## 2. 目标形态

- 新增子命令 `AskHuman mcp`：以 STDIO 运行 MCP server，暴露 `ask`、`whats_next`、
  `show_last`、`todo_add`、`todo_list`、`todo_update`；`ask` 覆盖现 CLI `AskHuman` 的全部提问能力。
- MCP server 为**薄壳**：每次 `ask` 调用就 spawn 一个现有的 `AskHuman …` 子进程（默认文本输出），复用全部既有 ask 流程（弹窗 / IM / 抢答 / 历史 / 落盘 / 排空与重连），结果区块文本原样透传，再把人类回复中的图片读回、转 `ImageContent` 直接返回给模型。
- 自动集成：每家 Agent 改为「**CLI | MCP | 未集成**」三态互斥选择。CLI 模式绑定 `Rule + 超时 Hook`，MCP 模式绑定 `Rule + MCP 配置`。
- 手动集成：参考提示词提供 **CLI 版 / MCP 版**两份可切换展示；MCP 版同时展示各家 **MCP 配置实例**。

## 3. 已确认决策

| 编号 | 决策项 | 结论 |
|---|---|---|
| D1 | 模式互斥 | 每个 agent 三态「CLI / MCP / 未集成」，互斥。同一 agent 不同时安装 CLI 与 MCP 产物（避免双触发/冲突） |
| D2 | MCP server 形态 | **薄壳**：不自带提问/弹窗/IM 逻辑；正常 `ask` 调用 spawn 现有 `AskHuman …` 子进程（默认文本输出）复用全流程。Codex 拦截来源（`system` / `ambient_suggestions`）thread 在 spawn 前本地拒绝 |
| D3 | 启动方式 | 新增 busybox 角色子命令 `AskHuman mcp`（与 `daemon`/`--popup`/`__agent-hook` 并列），用 `rmcp` 跑 STDIO server |
| D4 | 工具与 Schema | `ask` 入参为 `message`、`questions[{question, options[{text, recommended}]}]`、`files[]`；`whats_next` 承载完成报告/建议，`show_last` 对模型为零业务参数；`todo_add` 写项目待办，`todo_list` 只读列稳定 Todo/附件 ID，`todo_update` 按稳定 ID 增删附件。会话 token 字段可被托管 Hook 注入但必须从 `tools/list` schema 隐藏。`questions` / `options` item 直接内联，不得依赖本地 `$ref` |
| D5 | 输出（文本区块透传 + 图片直返）| `ask` **不声明 output schema**、不返回 `structuredContent`：子进程以默认文本模式调用，stdout 的结果区块（`[selected_options]` / `[user_input]` / `[files]` / `[status]`，多题 `# Qn` 分组）**原样透传**为 `content[0]` 的 `TextContent`，与 CLI 契约同一份字节。**取消时**即 `[status]` 区块引导文案（必须重新确认直到用户明确答复，不得当作放行）。区块格式说明放在工具 description（tools/list 每会话必达，替代 CLI 侧 `--agent-help` 的对应两节）。人类回复中的图片按 `[files]` 区块路径读出后以 `ImageContent`(base64+mimeType) 一并放入 `content` 直返模型；非图片文件以路径出现在 `[files]` 区块中。三个 Todo 工具返回包含稳定 ID 的文本结果。（二轮定案 2026-07-25，依据见 §10） |
| D6 | 超时 | MCP 模式**不需要超时 Hook**，但需按各家机制配置工具超时（否则长等待被取消）：**Codex** 写 `tool_timeout_sec=86400`(秒)+`startup_timeout_sec=30`；**Grok** 另写 `tool_timeouts = { ask = 86400 }`；**Claude Code(CLI)** 在 `mcpServers.askhuman` 写 `timeout=86400000`(**毫秒**,24h)；**Cursor** 工具/elicitation 超时 ~60s **硬编码不可配置**，不写 timeout（Cursor 推荐 CLI 模式） |
| D7 | MCP 配置落点 | **用户级全局**（与现有 Rules/Hook 一致）：Codex `~/.codex/config.toml`、Grok `~/.grok/config.toml`、Claude `~/.claude.json`（top-level `mcpServers`）、Cursor `~/.cursor/mcp.json` |
| D8 | 模式切换 | **一键切换**：切到另一模式时自动卸载旧模式全部产物，再安装新模式。选「未集成」= 卸载当前模式全部产物 |
| D9 | turn 生命周期 | 保持**正交**：turn 追踪仍只靠现有实验性 lifecycle hook，可与 MCP 模式并行独立开启，互不影响（MCP 拿不到 turn 周期） |
| D10 | 双版本提示词 | 新增 `prompts::mcp_reference()`：把「用 Shell 调 AskHuman、设 24h 超时、先跑 --agent-help」改为「调用 MCP 工具 `ask`」；其余交互纪律（必须提问、推荐选项、附件、结束前回执等）保留。手动集成卡支持 CLI/MCP 切换显示 |
| D11 | 自动重连 | MCP server **不持 ask 子流程的 daemon 长连接**：每次 `ask`/`whats_next` 都新起子进程→新走 `ensure_running`/排空等待/提交。daemon 更新后 MCP server 继续存活，后续调用自动连到新 daemon |
| D12 | 平台范围 | **macOS / Linux / Windows**。统一用 spawn 子进程：三平台子进程都是「瘦客户端→shared daemon」；传输差异留在 Unix socket / Windows named pipe adapter。MCP server 自身不直接弹窗，避免 STDIO 主循环与 Tauri 主线程冲突。 |
| D13 | 漂移检测 | `needs_update` 覆盖 MCP Rule + MCP 配置：已安装但内置提示词/配置模板有更新时显示「更新」 |
| D14 | CLI/doctor | `agents mode/update/show` 与 `doctor` 纳入 MCP 模式状态与整包操作（headless 一致可用） |
| D15 | 命名 | 子命令 `mcp`；工具名 `ask` / `whats_next` / `show_last` / `todo_add` / `todo_list` / `todo_update`；各家配置中 server 名 `askhuman` |
| D16 | 配置 command | 配置里的 `command` 写**当前可执行文件绝对路径**（`current_exe()`，与 Hook 脚本写绝对路径一致），因部分客户端不继承 shell PATH |

## 4. MCP server 运行流程（薄壳）

```
客户端(Codex/Claude/Cursor)
  └─ 按配置 spawn: <AskHuman 绝对路径> mcp        （STDIO，session 期常驻）
       └─ rmcp STDIO server，暴露 `ask` / `whats_next` / `show_last` / `todo_add` / `todo_list` / `todo_update`
            └─ 收到 ask 调用：
                 0. 检查 Codex turn metadata；拦截来源 thread 直接返回 terminal error
                 1. 入参 Schema → argv（message / -q / -o / -o! / -f；默认文本输出）
                 2. spawn 子进程: <AskHuman 绝对路径> <argv...>
                      · macOS/Linux/Windows：瘦客户端 → shared daemon
                        （弹窗/IM/抢答/历史/落盘/排空重连全复用）
                 3. 等子进程结束，读 stdout(结果区块文本) + exit code
                 4. stdout 原样作为 TextContent；按 `[files]` 区块路径读出图片转 ImageContent
                 5. 组 CallToolResult 返回
```

- **ask 主流程不变**：子进程仍是普通 CLI ask；上下文恢复只新增 MCP instance 注册与 Grok pending/claim 内部 IPC 消息。
- **并发**：客户端若并发调用 `ask`，各自 spawn 独立子进程，daemon 已支持并发请求（每请求独立 Coordinator）。
- **环境边界**：子进程保留 cwd 与普通环境，但先清除长驻 MCP server 可能继承的四家原生 session 变量；只用每调用的可信绑定写内部环境。

### Codex 内部 thread 前置边界

Codex 把 per-turn metadata 放在请求 `_meta["x-codex-turn-metadata"]`；AskHuman 兼容该值为 JSON
object 或 JSON 字符串，只对其中精确小写的 `thread_source ∈ {"system", "ambient_suggestions"}`
命中（前者为 ChatGPT.app 26.7xx 之前 ambient 线程的标签，后者为 26.721+ 的新标签）。不得信任顶层
任意同名 `thread_source`。字段缺失、格式错误、`user`、`automation` 或未知值均 fail-open，避免误伤
旧 Codex 与其它 MCP 客户端。

命中后 `ask`、`whats_next`、`show_last`、`todo_add`、`todo_update` 返回 `isError: true`；只读
`todo_list` 不产生副作用，可继续读取。固定错误文本为：

```text
AskHuman is disabled for this Codex system-generated background thread.
Do not retry or contact the human; finish the host-requested non-interactive output directly.
```

该返回不伪造 human answer 或结束批准；五个有副作用/交互的 handler 都在参数业务校验、spawn 子 CLI、项目检测与 todo
落盘前执行同一 guard。因此 Suggested prompts 即使按通用 Rules 尝试调用 AskHuman，也不会触发
popup、IM 或项目 todo。

命中时还向 `~/.askhuman/daemon.log` 追加一行 JSON 审计，固定包含
`event="askhuman_guard"`、`component="mcp_tool"`、`action="suppressed"`、
`reason="codex_blocked_thread_source"`、`threadSource`（观测原值）与工具名；可信 metadata 中存在时
附带 session/thread/turn id。日志不记录提问正文、参数或任意未识别 metadata。

带 `x-codex-turn-metadata` 但**未命中**拦截的调用不拒绝，但同样写一行 `action="passed"` 放行审计，
`reason` 取 `codex_thread_source_passed`（来源存在但不在拦截集合，`threadSource` 记录原值）、
`codex_thread_source_missing`（metadata 可读但无 `thread_source` 字符串）或
`codex_turn_metadata_unreadable`（metadata 无法解析）之一——宿主未来再改 ambient 标签时，新值直接
出现在放行日志里而不是无声 fail-open。非 Codex 客户端（`_meta` 无该命名空间）既不拦截也不写审计。

## 5. `ask` 工具 Schema（草案）

入参（JSON Schema，camelCase）：

```jsonc
{
  "message": "string?",                 // 所有问题的共享描述（可选）；恒按 Markdown 渲染（GFM）
  "questions": [                         // 省略或空时：message 作为单个问题（与 CLI 归一化一致）
    {
      "question": "string",
      "options": [                       // 可选预定义选项
        { "text": "string", "recommended": false }
      ]
    }
  ],
  "files": ["string"]                   // 可选，-f 展示附件（AI→人；绝对/相对/~ 路径）
}
```

不在 MCP 暴露的 CLI 开关：`--no-markdown`（MCP 恒 Markdown，不传该 flag）、`--single`、`--select-only`（脚本/纯文本专用，模型自助场景不适用）。

本地 WebView 对 `message` / `question` 中显式 ```` ```mermaid ```` fence 做渐进式图表渲染，范围和安全
边界见 `docs/plans/mermaid-rendering.md`。这是展示层扩展，不改变 MCP schema、子进程 argv、stdout 或
历史数据；Telegram、Slack、飞书、钉钉仍接收原始 Markdown fence，不新增图片、附件或网络渲染调用。

argv 映射：`message`→首个位置参数（或经 `-q` 拆分）；每个 question→`-q`；option→`-o`（`recommended` 时 `-o!`）；每个 file→`-f`。子进程保持默认**文本输出**（不传 `--output json`），以 argv 数组 spawn（无 shell，免引号转义）。

返回（文本区块透传，D5 二轮定案）：

- `content[0]` = 子进程 stdout 的结果区块文本**原样透传**（`[selected_options]` / `[user_input]` / `[files]` / `[status]`，多题按 `# Qn` 分组）——与 CLI 契约同一份字节，`whats_next` / `show_last` 亦为同款文本，四处契约统一；
- **取消 / 重放**都由 `[status]` 区块承载（取消引导要求模型必须重新确认直到用户明确答复；重放说明由 daemon 在区块前注入，见 spec duplicate-ask-coalescing D7），薄壳不解析不改写；
- 每张人类回复图片一个 `ImageContent`（从 `[files]` 区块按行取路径、图片扩展名过滤、读文件 → base64 + mimeType；读不到即跳过）；非图片回复文件仍以路径出现在 `[files]` 区块中；
- 不声明 output schema、不返回 `structuredContent`。区块格式说明写在 `ask` 的 description 里（~66 token，替代 outputSchema 的 ~345 token）。

退出码/动作映射：子进程 exit 0=已作答**或**用户取消（stdout 均有结果区块，原样透传为正常结果）、1=参数错误、3=系统错误（无可用渠道等）。仅当退出码非 0 或 stdout 为空（真正的执行错误，如连不上 daemon）才返回 MCP `isError` 结果并透传 stderr。

## 6. 自动集成 UI（设置「Agent」Tab）

- 每家 Agent（Cursor / Claude Code / Codex）从「分散的 Rule/Hook 卡」改为一个**三态模式选择**：`CLI | MCP | 未集成`（互斥）。
- 选中某模式后，其下展示该模式**绑定的产物**及安装/更新状态：
  - **CLI**：CLI 版 Rule + 超时 Hook（Codex 无 Hook，仅 Rule）。
  - **MCP**：MCP 版 Rule + MCP 配置（展示落点路径、可「打开/定位」）。
- 切换模式：一键（自动卸旧装新）。选「未集成」：卸载当前模式全部产物。
- 产物内容过期（提示词/配置模板更新）时显示橙色「更新」。
- 手动集成区：参考提示词卡支持 **CLI / MCP** 切换；MCP 版附**该家 MCP 配置实例**片段。

## 7. 各家 MCP 配置写入规范（用户级全局）

均使用「最小化编辑、保留用户其它内容、解析失败即中止不覆盖」的原则（复刻现有 hook/rule 集成做法），并以托管标记识别自有条目以便幂等更新/卸载。

- **Codex**：`~/.codex/config.toml`，`toml_edit` 写
  ```toml
  [mcp_servers.askhuman]
  command = "<AskHuman 绝对路径>"
  args = ["mcp"]
  startup_timeout_sec = 30
  tool_timeout_sec = 86400
  ```
- **Cursor**：`~/.cursor/mcp.json`（与 hooks.json 不同文件），`jsonc` CST 写。**不写 `timeout`**（Cursor 不认该字段且超时硬编码 ~60s 不可配）：
  ```json
  { "mcpServers": { "askhuman": { "command": "<绝对路径>", "args": ["mcp"] } } }
  ```
- **Claude Code**：`~/.claude.json` top-level `mcpServers`（用户级）。当前 Claude 版本已支持用户级加载（用户确认），按用户级写入，无需回退项目级。**额外写 `timeout`(毫秒)** 覆盖其 60s 默认：
  ```json
  { "mcpServers": { "askhuman": { "command": "<绝对路径>", "args": ["mcp"], "timeout": 86400000 } } }
  ```

## 8. 约束与既有规则（不可破坏）

- **不改对外契约**：stdout 洁净、结果区块、退出码、配置容错全部不变。MCP ask 路径继续经由「spawn 现有 CLI 子进程」复用；新 IPC 变体仅承载内部会话绑定。
- **互斥安装的幂等与最小化编辑**：所有配置写入只触碰自有托管条目，保留用户其它内容；解析失败中止、不整文件覆盖（沿用 `cursor_hook`/`claude_hook`/`agent_rules` 的纯函数 + 单测做法）。
- **CLI 模式行为完全不变**：现有 Rule/Hook 安装/更新/卸载逻辑保留，仅在 UI 与 `agents` 命令层并入「模式」抽象。
- **lifecycle hook 正交**：实验性 turn 追踪独立于 CLI/MCP 模式选择，不被互斥逻辑波及。
- **跨平台**：Windows 子进程经安全 named pipe 连接 shared daemon；MCP server 不直接持有 Tauri 主线程。

## 9. 验收标准

1. `AskHuman mcp` 启动 STDIO MCP server，`tools/list` 含 `ask`、`whats_next`、`show_last`、`todo_add`、`todo_list`、`todo_update`；`ask` input schema 直接保留 `questions[].question` 与 `questions[].options[].text`（无 `$defs` / `$ref`），三个交互工具的隐藏 token 字段均不出现在 schema；Todo 读写按稳定 ID 工作。
2. 在 Codex 中：写入 `[mcp_servers.askhuman]`（含大 `tool_timeout_sec`）后，调用 `ask` 能弹窗/经 IM 提问、长时间等待不超时；人类回复正常返回。
3. `ask` 覆盖核心能力：多问题、`options`/`recommended`、`files` 均按 CLI 语义生效；`message`/`question` 按 Markdown 渲染，本地 Popup 的显式 Mermaid fence 渐进渲染且四个 IM 渠道保持源码；取消时输出顶层 `status` 引导。
4. 人类回复图片：模型侧收到 `ImageContent`（可见图像），非图片文件以路径出现在文本中。
5. daemon 因版本更新 drain/重启：已运行的 MCP server 不退出，下一次 `ask` 自动连到新 daemon（撞排空时等待后成功）。
6. 设置「Agent」Tab：三态模式互斥；一键切换自动卸旧装新；选「未集成」清除全部产物；产物过期显示「更新」。
7. 手动集成：CLI/MCP 提示词可切换；MCP 版显示三家配置实例。
8. `agents mode/update/show` 与 `doctor` 正确反映 MCP 状态；旧逐产物 `--mcp` 写接口不再执行；headless 可用。
9. 三家 MCP 配置写入为最小化编辑：保留用户其它条目/注释；重复安装幂等；卸载只移除自有条目；解析失败不破坏文件（单测覆盖）。
10. Windows：`AskHuman mcp` 与 macOS/Linux 一样由子进程进入 shared daemon；提问、IM 抢答、历史、排空与 Agent 上下文语义一致，本地 IPC 使用用户私有 named pipe。
11. 既有 CLI 模式（Rule/Hook）与所有现有功能回归正常。
12. raw `tools/call` 携带 Codex `thread_source=system` 或 `ambient_suggestions` 时五个交互/副作用工具均返回固定
    terminal error 且无副作用；同时写入含工具名、`codex_blocked_thread_source` 原因与 `threadSource`
    原值的结构化审计；`user`、`automation`、缺失和异常 metadata 不命中拦截，但写 `action="passed"`
    放行审计。

## 10. 待实现期复核 / 开放细节

- Cursor / Claude Code 的 MCP 工具调用是否有超时上限、是否可配（Codex 已确认可配）。
- `ImageContent` 在三家客户端的实际渲染/喂模型表现（Codex 已确认 OK）。

> 已定（首轮）：`ask` 输出走 JSON / 结构化 + output schema（子进程 `--output json` → `structuredContent` + 序列化 JSON 文本 + `ImageContent`）；Claude 用户级 `~/.claude.json` 当前版本支持，无需回退。
>
> **已定（二轮，2026-07-25，推翻首轮输出形态）**：`ask` 与 Todo 工具改为**纯文本透传**（D5 现行文）。依据（o200k tokenizer 实测 + 客户端源码/文档核实）：
>
> - outputSchema 在 tools/list 中每会话固定 ~345 token，而等价格式说明放 description 仅 ~66 token；每次调用 JSON 比文本区块多 5–13 token（多题含未答题时 JSON 略省，但文本明确标出「用户未回答」信息量更足，采纳）；
> - Claude Code（官方 Agent SDK 文档）与 Codex（`as_function_call_output_payload`）在有 `structuredContent` 时都**只**把它喂给模型、丢弃 `content` 文本——「双份兼容文本」对模型无益、纯增传输；无 `structuredContent` 时两家都原样转发文本与图片块，Cursor/Grok 与 `whats_next` 现行纯文本同路径在产验证；
> - 对照 Claude Code 原生 `AskUserQuestion` 的 tool_result（`User has answered your questions: "…"="…"` 一行纯文本），官方内置提问工具同样不用结构化回传。

## 11. 反馈意见

- （待用户审阅后补充）
