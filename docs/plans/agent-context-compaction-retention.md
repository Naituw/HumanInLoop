# Agent 上下文压缩中的 AskHuman 问答恢复

> Windows 注（2026-08）：本文保留首期 Windows 单进程措辞；当前四家上下文恢复在 Windows 通过
> shared daemon、native caller binding 与 Windows hook command 工作，旧回退描述仅作历史记录。

> 状态：已实现并安装验证（2026-07-22）
> 范围：Codex、Claude Code、Cursor、Grok；AskHuman CLI/MCP 问答

## 结论先行

1. Codex 压缩上下文后，旧的 assistant 消息、reasoning、tool call 和 tool result 都不能
   假定继续原样存在。AskHuman 作为 MCP 工具时，其提问是 tool call，人的回答是 tool
   result，因此**精确内容可能丢失**；摘要可能保留语义，但没有逐字保留保证。
2. MCP 当前没有一种 annotation、`_meta`、XML 标签或 tool description 声明，能要求
   Codex/Claude/Cursor 在压缩时永久保留某次工具调用及结果。
3. 用户提出的恢复方案可行：同时新增 CLI `AskHuman --show-last` 与 MCP `show_last`，优先按
   当前 Agent session ID 查询这条会话最后一次完成的 AskHuman 问答，并输出完整问题与答案。
4. **上下文压缩不会创建新的 Agent session。** 压缩前后的 Codex `thread_id`、Claude
   `session_id`、Cursor `conversation_id` 保持不变。三者是各家对会话键的不同命名，恢复
   方案不需要再创造一个独立的 `conversation_id`。
5. 当前 AskHuman 已经在 CLI 请求中探测并传递 `agent_session_id`，但 `HistoryEntry` 没有
   保存它，`Coordinator` 也没有接收它。这是 `--show-last` 的主要数据缺口。
6. MCP 协议自身没有“当前 Agent session ID”标准字段。Codex 每次 MCP 请求自带
   `_meta.threadId`；Claude/Cursor 则由 PreToolUse hook 给 `ask`、`whats_next`、`show_last`
   统一注入一个 schema 隐藏、短命且一次性的 AskHuman token，handler 用它解析真实 session。
   token 失败时再按已确认的 best-effort 语义退回 MCP instance；不能依赖 MCP server 启动时
   继承的 Agent session 环境变量。Grok 0.2.106 实测不支持修改 hook input，但 PreToolUse 能
   上报真实 session 与参数指纹；首版用安全旁路做“唯一候选时精确认领”，否则退回 MCP instance
   best-effort。
7. 上述 PreToolUse 绑定与 Codex/Claude compact 提示由**集成模式托管的独立恢复 Hook**提供，
   不依赖可选的 lifecycle tracking。恢复 Hook 使用独立 marker/隐藏入口，不改现有
   `AskHuman __agent-hook <agent> <event>` 命令；二者可以同时存在。Codex 新 Hook 的安装、更新、
   删除都必须同步维护 `hooks.json` trust identity 与 `config.toml` trusted hash，失败时双文件回滚。

## Codex 源码分析

源码基于 `/Users/wutian/Developer/codex` 当前工作区。

### 本地压缩

`codex-rs/core/src/compact.rs` 的核心流程是：

1. 让模型生成摘要；
2. 从旧历史中单独收集“真实 user message”；
3. 在 token 预算内保留最近的真实 user message；
4. 追加压缩摘要；
5. 用这组 `new_history` 替换原 history。

`collect_user_messages()` 只接受能解析成 `TurnItem::UserMessage` 的条目。工具调用、工具
结果、assistant reasoning 等不会进入这个确定性保留集合。`build_compacted_history_with_limit()`
还会按 `COMPACT_USER_MESSAGE_MAX_TOKENS` 从最近消息反向选取，过长的 user message 也可能
截断。

因此，本地压缩后的确定性形态可以概括为：

```text
有限预算内的真实 user messages
+ 当前 canonical initial context（按压缩时机重新注入）
+ 一条模型生成的 summary
```

AskHuman MCP 调用和结果不属于“真实 user message”，只能期待摘要模型概括它们，不能期待
原始 JSON、问题选项、推荐标记、自由文本和附件路径都还在。

### 远程压缩

`codex-rs/core/src/compact_remote.rs` 的 `should_keep_compacted_history_item()` 明确丢弃：

- reasoning；
- local shell/function/custom tool call；
- function/custom tool output；
- tool search call/output；
- 其它非白名单 history item。

它保留远程压缩结果中的有效 user message、hook prompt、assistant/agent message 和 compaction
item，但这不等于保留压缩前的原始工具交互。

`codex-rs/core/src/compact_remote_v2.rs` 更直接：输入历史中只挑选 user/developer/system
message 作为额外 retained messages，再由上述过滤器清理并按预算截断，最后追加服务端返回的
compaction output。原始 MCP tool call/result 不在 retained 形态中。

所以无论走本地还是当前远程 V2 压缩，都不能把 AskHuman 工具结果视为稳定保留项。

### 压缩后的注入点与会话 ID

`Session::replace_compacted_history()` 替换 history 后，会排队：

```rust
SessionStartSource::Compact
```

下一次模型侧继续执行前，`run_pending_session_start_hooks()` 会运行匹配 `compact` 的
SessionStart hook，并把 hook 的 `additionalContext` 记入后续上下文。源码测试
`compact_session_start_hook_records_additional_context_for_next_turn` 验证了该内容确实出现在
压缩后的下一次模型请求中。

这个过程始终调用同一个 `Session` 对象；没有创建新 thread。hook input 中的
`session_id` 也取自当前 `sess.session_id()`。因此压缩不会改变 Codex thread/session ID。

此外：

- Shell/runtime 子进程会得到 `CODEX_THREAD_ID`；
- 每次 MCP 调用都会由 `codex-rs/core/src/mcp_tool_call.rs` 的
  `with_mcp_tool_call_thread_id_meta()` 写入 `_meta.threadId`，并覆盖同名旧值。

这两点分别满足 `AskHuman --show-last` 的读取端和 AskHuman MCP `ask` 的可靠写入端。

## Claude Code 与 Cursor 对照结论

### Claude Code

Claude Code 的压缩会用摘要替代旧上下文，工具结果没有原样保留契约。官方 hook 当前提供：

- `PreCompact`：压缩前；
- `PostCompact`：压缩后，仅适合审计/副作用，不能修改压缩结果；
- `SessionStart` matcher `compact`：压缩后触发，stdout 或 `additionalContext` 会加入 Claude
  上下文。

所有 hook 的公共输入都包含当前 `session_id`；`SessionStart(source=compact)` 仍是同一个
Claude 会话。`/clear`、切换或 fork 才是另一个会话语义，compact 本身不是。

因此 Claude 可在 `SessionStart(compact)` 注入一条**按当前集成模式生成**的短提示：CLI 模式
要求运行 `AskHuman --show-last`，MCP 模式要求调用 AskHuman MCP `show_last`。

官方参考：

- <https://code.claude.com/docs/en/hooks>
- <https://code.claude.com/docs/en/hooks-guide>

### Cursor

本机 Cursor Desktop/CLI 静态分析确认它有多种压缩器，保留边界并不相同：

| 压缩实现 | 确定性保留 | AskHuman tool call/result |
| --- | --- | --- |
| external partial summarizer | 从最近真实 user message 开始的 tail | tail 内可能原样保留 |
| external manual full | 无上述 tail | 只可能进入摘要 |
| generic self-summary | 最后一个非 summary user message | 不原样保留 |
| Anthropic compact | 无普通历史 tail | 不原样保留 |
| OpenAI compact | 预算内的真实 user messages | 不原样保留 |

四类结果正文都有明显的 summary/compaction 提示，所以后继模型能够知道自己“刚被摘要”；
Cursor Rules 又位于压缩时稳定重建的静态 instruction 区域。因此 Cursor 不必依赖不存在的
PostCompact 注入点：CLI Rule 提醒运行 `AskHuman --show-last`，MCP Rule 提醒调用 AskHuman MCP
`show_last`。

Cursor 压缩只替换当前 conversation 的 messages，没有重建 conversation config；Shell
环境中的 `CURSOR_CONVERSATION_ID` 也由同一 conversation ID 注入。因此压缩前后 ID 不变。

Cursor 当前没有能把动态内容可靠追加进压缩 replacement messages 的 PostCompact hook；
`PreCompact.user_message` 只进入 UI 状态，不应作为恢复方案。

### Grok 实测结论

本机 Grok `0.2.106`（build `bde89716f679`）的随附 `10-hooks.md` 只定义 PreToolUse
`allow/deny`。为排除文档滞后，2026-07-22 做了两个隔离真机探针：

1. 普通 `run_terminal_command`：hook 收到真实 `sessionId`、`toolUseId` 和完整 `toolInput`，
   返回 `{"decision":"allow","updatedInput":...}`，实际仍执行模型原始命令；
2. 临时 stdio MCP `session_probe`：hook 看到精确限定名
   `grok_session_probe__session_probe`、真实 session/tool-use ID，并尝试把 marker 改成
   `UPDATED_FROM_HOOK`；server 收到的仍是模型原始 `ORIGINAL_MCP_MARKER`。

抓到的真实 `tools/call` 形态为：

```json
{
  "method": "tools/call",
  "params": {
    "_meta": { "progressToken": 1 },
    "name": "session_probe",
    "arguments": { "marker": "ORIGINAL_MCP_MARKER" }
  }
}
```

它没有 session/tool-use `_meta`；stdio server 环境也没有 `GROK_SESSION_ID`。探针反而继承了
启动 Grok 的上层 `CODEX_THREAD_ID`，进一步证明 MCP handler 必须主动清除所有继承 session
环境，不能据此识别 Grok。

所以当前 Grok 存在“hook 知道 session，但无法把关联键送进 MCP request”的断点。不能用“参数
hash + 时间窗取第一条”或 FIFO；并行相同参数会竞态。可以采用安全旁路：PreToolUse 把真实
session 与 canonical arguments hash 上报 daemon，MCP handler 只在同一
`mcp_instance_id + project` 永久分区内候选恰好一条时原子认领；0 条或多条都不猜，退回该
instance/project 的 best-effort。

这能在普通单调用场景恢复真实 Grok session，又不会把歧义候选误绑；但它仍不是协议级 100%
通道。未来若 Grok 支持 updatedInput、把 session 写进 `_meta`，或 AskHuman 改成可利用
`{{session_id}}` header 的 Streamable HTTP transport，再替换旁路关联。

## 为什么前一版会讨论 `conversation_id`

那不是因为压缩会改变 ID，而是因为前一版把恢复设计成 MCP tool。长驻 MCP server 可能被
多个 conversation 复用，恢复工具如果只看 server 启动时的环境，就可能拿到旧会话；所以前
一版让 Cursor hook 把当前 `conversation_id` 注入工具参数。

CLI 恢复使用 `AskHuman --show-last` 后，这个公开 session 参数完全可以删掉：

```text
压缩后的 Agent
  -> 调用 Shell: AskHuman --show-last
  -> Shell 子进程读取当前 CODEX_THREAD_ID / CLAUDE_CODE_SESSION_ID /
     CURSOR_CONVERSATION_ID / GROK_SESSION_ID
  -> history 按 (agent_kind, agent_session_id) 查询最后一条完成问答
```

也就是说：

- 读取端不需要模型提供 ID；
- MCP `show_last` 同样不让模型提供 ID，而由 Codex `_meta` 或 Claude/Cursor hook token 绑定；
- 不需要 `restore_pending`、compaction sequence 或 decision ledger；
- 只需要确保写入 history 时把每次 `ask` / `whats_next` 标上正确归属键。

## 最小产品方案

### 1. 新 CLI 命令与 MCP 工具

新增：

```bash
AskHuman --show-last
```

CLI 应当：

1. 用现有 `detect_caller_agent()` / `session_id_env_var()` 得到当前 `agent_kind` 与
   `agent_session_id`；
2. 如果检测到 Agent session，只查询 `action == Send` 且二者精确匹配的最新
   `HistoryEntry`；精确查询无结果时不再级联到弱键；
3. 如果完全不在 Agent 内调用，则按当前 project 返回最后一条 `action == Send` 的 history；
4. 输出 Message，然后输出每一道完整 question（含全部选项和 recommended 状态）及其完整
   answer（选择、自由文本、图片/文件路径）；
5. 这是普通只读 CLI，不弹窗、不走 daemon、不产生新 Agent user turn。

同时在 AskHuman MCP server 新增零公开业务参数的 `show_last` 工具。它与 CLI 复用同一个查询
和格式化核心；内部允许 hook 注入 schema 隐藏字段 `__askhuman_session_token_v1`。查询优先级为：

```text
有效的 Codex _meta.threadId / Claude-Cursor hook token
  -> 精确 (agent_kind, agent_session_id)
  -> 若根本拿不到真实 session，则 best-effort 查询 (mcp_instance_id, project)
  -> 无结果
```

已经取得真实 session、但该 session 没有 history 时不继续降级；只有真实 session 完全不可得
时才使用 MCP instance 弱键。运行时提示严格服从用户选择的集成模式：CLI Rule 只写 CLI，MCP
Rule 只写 MCP。设置页的“推荐”标签不进入任何运行时提示词。

恢复输出复用 CLI 的纯区块风格，但只携带继续推理需要的提问语境与实际答案；空字段、未选
候选项和 recommended 标记不输出。Message/每道问题是同级语义块，以 `---` 分隔：

```text
[message]
<非空短 Message 的全文>

[message_files]
<非空提问附件路径，每行一个>

---

[question]
<完整问题>

[answer_selected_options]
<人类实际选中的选项>

[answer_user_input]
<人类实际输入>

[answer_files]
<人类回复的图片/文件路径，每行一个>
```

如果 Message 超过约定阈值：

- `[message_truncated]` 输出一段前缀；
- 把 Message 全文写到 AskHuman 管理的私有文件；
- `[message_full_file]` 输出该文件的绝对路径；
- questions 与实际 answers 仍完整输出，不做摘要；未回答题使用
  `[answer_status]\nunanswered`。

文件建议按当前 session（非 Agent CLI 则按 project）覆盖写，而不是每次产生永久新文件；权限
应限制为当前用户可读。阈值已确认：Message 按 UTF-8 超过 8 KiB 时写私有文件，stdout 保留
2 KiB 前缀和全文绝对路径。

### 2. History 增加会话键

`src-tauri/src/history.rs` 的 `HistoryEntry` 新增：

```rust
#[serde(default, skip_serializing_if = "Option::is_none")]
pub agent_session_id: Option<String>

#[serde(default, skip_serializing_if = "Option::is_none")]
pub mcp_instance_id: Option<String>
```

旧 JSONL 可继续读取；旧记录的字段为 `None`，不能参与 session/MCP instance 精确匹配，但可在
完全非 Agent 的 CLI project fallback 中作为普通 project history 被读取。

同时把 `TaskRequest.agent_session_id` 传入 `Coordinator::new_ipc()` / `Coordinator::build()`，
由 `record_history()` 写入 entry。当前代码已经在 `RequestRegistry::create()` 前拿到了这个值，
只是没有继续传给 Coordinator。

`try_whats_next_auto()` 等直接构造 `HistoryEntry` 的路径也需要填写探测到的 session ID；MCP
spawn 的子进程还要通过内部环境把当前 `mcp_instance_id` 传入 `TaskRequest` / Coordinator。

### 3. MCP 每次调用的 session 绑定

这是唯一不能只靠给 HistoryEntry 加字段就完全解决的地方。

#### MCP 到底能不能直接获取当前 session ID

不能把它当作 MCP 的通用能力。这里有三个容易混淆的 ID：

| ID | 谁定义 | 表示什么 | 能否用于 `--show-last` |
| --- | --- | --- | --- |
| JSON-RPC request ID | MCP | 单次请求 | 不能，不等于 Agent session |
| `Mcp-Session-Id` / transport session | MCP HTTP transport | MCP 客户端与 server 的连接/恢复会话 | 不能，不等于 Agent conversation |
| thread/session/conversation ID | Codex/Claude/Cursor | Agent 对话会话 | 可以，这是所需的键 |

MCP 规范允许客户端用 `_meta` 发送自定义元数据，但没有规定客户端必须发送 Agent session
ID。因此能否直接取得，取决于具体客户端：

- **Codex：可以。** `execute_mcp_tool_call()` 在每次调用前执行
  `with_mcp_tool_call_thread_id_meta()`，把当前 thread 写成 `_meta.threadId`，而且会覆盖
  传入的旧同名值。AskHuman 使用的 rmcp 支持在 tool handler 参数中提取 `Meta`，所以可以
  直接读取并显式设置子进程 `CODEX_THREAD_ID`。
- **Claude Code 2.1.205：不可以直接取得。** 普通模型侧 MCP 调用实际发送
  `callTool({ name, arguments, _meta })`，但它构造的 `_meta` 只有
  `{"claudecode/toolUseId": ...}`，没有 session ID。stdio MCP server 启动时确实得到
  `CLAUDE_CODE_SESSION_ID: Ct()`；但 `/clear` 会生成新 session 并更新 Claude 父进程的
  `process.env.CLAUDE_CODE_SESSION_ID`，代码只主动重连名为 `ide` 的 MCP client，普通长驻
  MCP server 不随之重启，所以其启动环境会变旧。
- **Cursor CLI/Desktop 当前版本：不可以直接取得。** `McpSdkClient.callTool()` 实际发送
  `this.client.callTool({ name, arguments })`，没有 `_meta`。stdio MCP client 由全局
  `clientCache` / lease 复用，启动环境只来自 MCP 配置的 `env`；当前 conversation ID 没有
  被逐调用送到 server。Cursor 自己的 hook context 虽然知道 `conversation_id`，但 MCP wire
  path 没有转发它。

这也解释了为什么“compact 后恢复”本身没问题，而“跨 session”需要额外处理：compact 不换
ID；`/clear`、resume 到另一会话或切换 Cursor conversation 才会让长驻 MCP 进程与当前 Agent
session 脱节。

#### 已确认绑定协议：Codex `_meta`，Claude/Cursor 统一 PreToolUse token

三种 MCP 工具统一处理：现有 `ask`、`whats_next`，以及新增 `show_last`。模型看到的 public
schema 不包含 session 参数；Claude/Cursor hook 在模型生成正常输入之后加入 AskHuman 内部
字段：

```text
__askhuman_session_token_v1
```

该字段只携带随机 token，不携带裸 session ID。Codex 仍优先使用客户端原生的
`_meta.threadId`，不需要 token。

##### 安装归属：独立恢复 Hook，不依赖 lifecycle tracking

恢复能力必须跟随用户当前选择的集成模式，不能复用当前可单独关闭的 experimental lifecycle
tracking。实现新增独立 marker 与隐藏入口，例如：

```text
AskHuman __context-recovery-hook <agent> <event>
```

它不修改、迁移或接管现有 `AskHuman __agent-hook <agent> <event>`。两套 handler 在同一事件下可
并存；关闭 lifecycle tracking 不删除恢复 Hook，切到 `None` 也不删除 lifecycle Hook。模式对应
的恢复事件矩阵固定为：

| Agent | CLI 模式 | MCP 模式 |
| --- | --- | --- |
| Claude Code | `SessionStart`：仅 compact 时提示 CLI 恢复 | `SessionStart`：仅 compact 时提示 MCP 恢复；`PreToolUse`：token 绑定 |
| Codex | `SessionStart`：仅 compact 时提示 CLI 恢复 | `SessionStart`：仅 compact 时提示 MCP 恢复；session 直接用 `_meta.threadId` |
| Cursor | 无恢复 Hook，依靠 CLI Rule | `preToolUse`：token 绑定；compact 仍依靠 MCP Rule |
| Grok | 不支持 CLI 模式 | `PreToolUse`：安全旁路 pending 上报；compact 仍依靠 MCP skill |

`agent_mode::set/update` 负责按目标模式幂等 reconcile；切换模式时先写目标 Rule/Skill 和对应
MCP/CLI 产物，再把恢复 Hook reconcile 到目标矩阵，`None` 则只删除恢复 marker。恢复 Hook 的
缺失/过期并入当前模式的更新状态，不在设置页新增独立一行：CLI 下归入现有 Hook artifact（Codex
原“无 Hook”提示需改为可显示恢复 Hook），MCP 下归入 MCP artifact，单项更新和“全部更新”都要
同时 reconcile 内部恢复 Hook。

Codex 的恢复 Hook 是受信任 Hook。每次新增、更新、删除恢复条目，都必须：

1. 同时保存原始 `hooks.json` 与 `config.toml` 字节；
2. 用现有通用 `reconcile_codex_trust()` 按真实 event、matcher、command、timeout 和数组索引计算
   canonical identity/trusted hash，并把恢复 marker 纳入受信列表；
3. 任一步失败时原样恢复两个文件；
4. 状态检查把恢复条目数量/正文/结构错误或 trust 缺失、失配都判为 outdated。

CLI 与 MCP 模式之间的提示差异由恢复 Hook 入口运行时读取当前 `agent_mode` 生成，不写入命令
参数，因此模式切换本身不需要改变 Codex command identity/trust hash。生命周期 Hook 的既有
trust 行为与命令正文保持不变，升级和降级都不会把两类 Hook 误认成对方。

##### A. 一次调用的完整时序

```text
Claude/Cursor PreToolUse
  -> 从 hook stdin 取得当前 session_id/conversation_id
  -> 生成 128-bit 随机 token
  -> 私有临时记录 token -> {agent_kind, session_id, tool_name, expires_at}
  -> updatedInput 注入 __askhuman_session_token_v1

AskHuman MCP handler
  -> 原子消费 token（校验 TTL、agent、tool name）
  -> 得到真实 agent session
  -> ask/whats_next: 带 session 启动 CLI，history 写入时即绑定
  -> show_last: 按真实 session 精确读取 history
```

临时记录建议放在 AskHuman 私有 state 目录，权限限制为当前用户；文件名只接受标准 UUID，先写
临时文件再原子 rename。handler 通过原子 rename/删除保证单次消费，创建和消费时顺手清理过期
记录。TTL 建议 30 秒。这样不要求 hook 执行时 daemon 已经启动，也不会把真实 session 写进
Agent transcript。

handler 消费成功后，用只在 `ASKHUMAN_FROM_MCP=1` 时受信的内部环境变量，把
`agent_kind + agent_session_id + mcp_instance_id` 交给 CLI/`TaskRequest`。启动子进程前先删除
四家的原生 session 环境变量，避免长驻 MCP server 启动时继承的旧值覆盖当前 token。

##### B. Public schema 与 hidden 参数

模型不需要、也不应看到内部字段。实现可用独立 public schema，或让 handler 的内部参数结构
包含：

```rust
#[serde(default, rename = "__askhuman_session_token_v1")]
#[schemars(skip)]
session_token: Option<String>
```

rmcp handler 仍能反序列化 hook 加入的字段，而 `tools/list` 发给模型的 schema 不列出它。
字段虽然不在 schema 中，仍可能出现在 Agent 的实际 tool-call transcript，所以必须使用短命
一次性 token，不能直接注入 session ID。

Claude 与 Cursor 的 hook 输出不能共用模板：

- Claude `updatedInput` 是替换语义：必须完整复制原始 `tool_input`，再加入 token；
- Cursor 当前实现是 merge 语义：只输出 token 字段即可。

当前 Cursor 实现已确认 merge 后直接进入 MCP args，没有可见的二次 schema 校验。Claude 的
schema 外字段必须用真实 MCP 调用做集成测试；若未来客户端升级后重验并拒绝，走下述
best-effort 或切换 CLI。

##### C. Cursor 的 server-name 盲区与已接受风险

Cursor 的真实调用顺序是：

```text
preToolUse(MCP:<toolName>，有 conversation_id、能改 input、没有 server 名)
  -> beforeMCPExecution(有 mcp_server_name、不能改 input，只消费 permission)
  -> tools/call
```

因此 Cursor hook 只能按 `MCP:ask` / `MCP:whats_next` / `MCP:show_last` 识别。为降低误注入：

- 只匹配这三个精确名字；
- 同时校验输入结构是否符合 AskHuman public schema；
- 使用极具体的字段名 `__askhuman_session_token_v1`。

风险已由用户接受：其它 MCP server 若恰有同名工具，宽松参数解析通常会忽略隐藏字段；严格
`additionalProperties: false` 的 server 可能让那一次调用报 `invalid params`。特殊字段名只能
避免语义冲突，不能让严格校验器接受未知字段。该风险主要影响其它同名工具的兼容性，不会让
AskHuman 的 session 串线。用户若在 Cursor MCP 模式遇到该兼容问题，可以主动切换到 CLI 模式；
这是一条故障处置说明，不写进 MCP 模式的 Agent Rule。

`beforeMCPExecution` 不能代替上述 token 注入：本机 Cursor 代码只读取它返回的 `permission`，
不会读取 `updated_input`；其输入又没有一个同时出现在 MCP request 中的 `tool_use_id`，用参数
hash/FIFO 做旁路关联在并行空参数 `show_last` 下会竞态。

##### D. Codex 原生路径

1. 三个 MCP handler 都增加 rmcp `Meta` context extractor。
2. 从 `_meta.threadId` 取值；Codex 源码保证每次调用前由当前 thread 覆盖写入。
3. 合法的 Codex thread ID 优先级高于隐藏 token和 MCP instance fallback。
4. handler 清除继承的旧 session 环境，只给子进程设置权威 Codex 绑定。

##### E. MCP instance best-effort

stdio MCP 没有 HTTP `Mcp-Session-Id` header。AskHuman 在每个 MCP server 进程启动时生成一个
随机 `mcp_instance_id`，并在 `AskServer` clone 间共享；每次 MCP `ask` / `whats_next` history
都记录它。

当 `_meta` 和 hook token 都完全不可得时：

- `ask` / `whats_next` 仍正常工作，history 至少记录 `mcp_instance_id + project`；
- MCP `show_last` 查询同一 `mcp_instance_id + project` 下最后一条 `action == Send`；
- MCP server 重启后 instance ID 改变，不跨新旧进程猜测；
- 同一 MCP client 被多个 conversation 复用时可能返回另一 conversation 的最近问答，这是
  明确接受的 best-effort 弱隔离，不伪装成真实 Agent session。

查询一旦取得真实 session，就只做精确 session 查询；精确查询没有记录时不继续降级。只有
根本无法取得真实 session 时才使用 MCP instance。

##### F. Grok 安全旁路认领

Grok 不走 hidden token。MCP 模式托管的独立恢复 PreToolUse Hook 对三种限定工具名增加 pending
binding 上报；是否开启 lifecycle tracking 不影响它：

```text
agent_kind = grok
agent_session_id = hook sessionId / GROK_SESSION_ID
qualified_tool_name = askhuman__ask | askhuman__whats_next | askhuman__show_last
arguments_sha256 = sha256(versioned canonical JSON of real arguments)
project = canonical project key
hook_parent_hint + tool_use_id + created_at
```

Grok Build 实测 MCP wrapper 为 `toolInput = {tool_name, tool_input}`；先验证外层 `toolName` 与
`toolInput.tool_name` 指向同一限定工具，再取 `toolInput.tool_input` 作为真实 arguments。若
Composer/未来版本传直接
arguments，可支持经过严格 shape 判定的第二种解析分支。`toolInputTruncated == true` 时不生成
候选。

AskHuman MCP instance 在启动/initialize 时向 daemon 注册：

```text
{mcp_instance_id, project, server_pid, parent_pid_hint, created_at}
```

handler 在三种工具入口、执行 history 查询或 spawn CLI 之前，发送 tool + canonical hash +
instance/project 做 claim。daemon 的认领顺序固定为：

1. **永久硬分区**：current `mcp_instance_id + project`；通过双方进程 hint/walk 只关联到同一
   Grok agent process 的 pending 候选；
2. 在分区内匹配限定 tool name、arguments hash 与短 TTL；
3. 候选恰好一条时原子删除并返回其真实 session；
4. 0 条或多条时返回 ambiguous/unavailable，绝不 FIFO/取最近，handler 走该 instance/project
   的 best-effort。

hook IPC 写完才退出，但 daemon 消费仍可能存在很小调度竞态；claim 可做一次几十毫秒的有界
重试。pending 只存 hash，不存问题正文；到期自动清理。process/project 关系解析失败只造成精确
命中减少，不得放宽过滤产生错绑。

`mcp_instance_id + project` 永久用于**认领候选分区**。一旦唯一认领得到真实
`agent_kind + agent_session_id`，最终 history 查询只按真实 session；不再 AND 当前 instance，
否则 MCP server 在同一 Agent session 中重启后会错误丢失重启前 history，CLI 也无法复用。

##### G. CLI 与失败语义

- CLI 集成模式下运行 `AskHuman --show-last`：从每次 Shell 执行的当前环境取真实 session，只做
  精确查询。
- 完全不在 Agent 内的普通 CLI：按当前 project 返回最后一条 `action == Send`，作为用户确认的
  best-effort 行为。
- hook 缺失、被禁用、超时，或 Claude 输入超过 hook 当前 10 MiB 完整解析上限：不生成
  token，MCP 走 instance fallback；CLI 不受影响。
- token 无效、过期、已消费或 tool name 不匹配：不得当作真实 session；记诊断后按“真实
  session 不可得”处理。
- history limit 为 0、条目已裁剪或旧记录缺少所需键时返回无结果，不绕过 history 设置。
- MCP 客户端取消或人类取消不改变绑定规则；`show_last` 只返回 `action == Send`。

Grok 已实测不能更新 MCP 入参，也没有 per-call session `_meta`；旁路唯一认领成功时使用真实
session，否则进入 MCP instance best-effort。Grok 当前产品集成只有 MCP 模式，不新增未经验证
的 CLI 推荐。Grok Build 已真机覆盖；Composer 的 wrapper shape 与 process 关联留作实现回归，
失败即安全降级。

独立恢复 PreToolUse 与现有 lifecycle PreToolUse 同时安装时必须做组合回归：用户自有 Hook 全部
保留；AskHuman 两条 marker 各自最多一条；Claude/Cursor lifecycle interjection 的 deny 必须
优先阻止工具执行，恢复 Hook 不得用 allow 覆盖 deny。若两条 Hook 并行运行而先创建了未消费
token/pending，只允许其按短 TTL 自行过期，不得把它转用于下一次调用。

### 4. Agent 规则

“推荐模式”与“已选模式提示词”是两件独立的事。当前产品矩阵为：

| Agent | 可选模式 | 设置页推荐 |
| --- | --- | --- |
| Cursor | None / CLI / MCP | CLI |
| Claude Code | None / CLI / MCP | CLI |
| Codex | None / CLI / MCP | MCP |
| Grok | None / MCP | MCP |

推荐值只控制设置页标签，不得进入托管 Rule/Skill。`agent_mode::set()` 已按模式安装
`Variant::Cli -> prompts::cli_reference()` 或 `Variant::Mcp -> prompts::mcp_reference()`；Grok
skill 恒复用 MCP reference。新增恢复纪律也必须进入这两个独立 prompt source，不能写成“优先
CLI”“可等价调用 MCP”或同时列两种入口。

CLI 变体增加：

```text
If you were just summarized, or if you are unsure of the exact details of the last
question you asked the user through AskHuman and their answer, run
`AskHuman --show-last` before continuing.
```

MCP 变体增加：

```text
If you were just summarized, or if you are unsure of the exact details of the last
question you asked the user through AskHuman and their answer, call the AskHuman
MCP `show_last` tool before continuing.
```

两者语义相同、入口严格互斥：

- 刚压缩：主动查一次；
- 没刚压缩但对上一次问答细节不确定：主动查一次；
- 已清楚掌握且未压缩：不要求重复查。

### 5. 压缩后短提示

Codex 与 Claude 有可靠的 compact 事件注入点，因此再加一条更直接的运行时提示。该
提示必须在 hook 运行时读取当前 `agent_mode`，按模式二选一；None 模式不输出恢复提示。

CLI 模式：

```text
You were just summarized. Run `AskHuman --show-last` now to retrieve the full last
AskHuman question and answer before continuing.
```

MCP 模式：

```text
You were just summarized. Call the AskHuman MCP `show_last` tool now to retrieve
the full last AskHuman question and answer before continuing.
```

- Codex：`SessionStart(source=compact).additionalContext`；
- Claude：`SessionStart` matcher `compact` 的 stdout/`additionalContext`；
- Cursor：没有等价的可靠压缩后注入点，依靠当前模式的 always-apply Rule 和摘要正文标记；
- Grok：现有压缩后 hook stdout 不进入模型，依靠 MCP skill 中的 MCP 变体规则。

短提示本身不携带问答内容，不会无限增长；真正内容只在需要时通过当前模式对应的 CLI/MCP
读取端取得。

## 方案能保证什么

能保证：

- 不依赖压缩摘要是否正确复述 AskHuman；
- 不需要特殊 MCP retention annotation；
- 不新建计费 user turn；命令发生在原 Agent 的工具循环中；
- 支持同一 session 多次压缩，每次都能查到同一 session 最新完成问答；
- `_meta` / hook token 有效，或 Grok 旁路候选唯一认领时，多 session 按真实 session 精确隔离。

不能绝对保证：

- 模型一定遵守静态 Rule 并执行当前模式对应的恢复入口；Codex/Claude 的 compact hook 提示
  更强，Cursor 仍是模型遵循约束；
- history 被关闭或记录已被裁剪后仍可恢复；
- 旧版未带 `agent_session_id` 的历史可安全补配；
- hook token 不可用且退到 `mcp_instance_id` 时，多 conversation 共用同一 MCP client 可能读取
  同 project 的另一 conversation 最近问答；这是显式 best-effort；
- Grok 0.2.106 的旁路在唯一候选时能精确认领，但协议本身不带关联键；候选为 0/多条或进程
  关系解析失败时仍会降为 MCP instance best-effort；
- Cursor 其它 server 恰有同名工具且严格拒绝未知字段时完全无兼容影响；此时应切换 CLI；
- 问答附件原文件被用户删除后仍可读取内容（路径仍可返回）。

## 实现结果

- `HistoryEntry` 已兼容新增 Agent session/MCP instance 归属键，`Coordinator` 与自动
  whats-next 直写路径都会保存它们。
- CLI `AskHuman --show-last` 与 MCP `show_last` 共用精确查询/格式化核心；恢复载荷只含非空
  Message/附件、问题与实际答案，不回放未选候选项；超过 8 KiB 的 Message 以
  `[message_truncated]` 2 KiB UTF-8 前缀 + `[message_full_file]` 0600 全文覆写文件返回。
- Codex `_meta.threadId`、Claude/Cursor 一次性隐藏 token 与 Grok 唯一候选旁路已接入
  `ask` / `whats_next` / `show_last`；长驻 MCP 进程继承的原生 session 环境会在启动子 CLI 前清除。
- 独立恢复 Hook 已按 None/CLI/MCP 模式 reconcile，与 lifecycle marker 共存；Codex trust
  和 hooks/config 失败回滚沿用通用可信 Hook 实现；无变化时不改写文件，非法 UTF-8/JSON
  会中止且保留原配置。
- 设置页的 Hook/MCP 产物更新状态已包含恢复子产物，不新增独立设置行；
  CLI/MCP Rule 与 compact 短提示只指向当前已选模式；CLI 的安装状态仅反映必需 timeout Hook，
  恢复子产物只参与聚合更新状态。
- CLI 子进程与 Windows 单进程回退统一从可信内部环境构造 caller context，确保 history 在各平台
  都保留真实 Agent session/MCP instance 绑定，并清除 MCP 长驻进程继承的陈旧原生 session 环境。
- 全面复核后的验证通过：Rust 959 tests（958 passed / 1 ignored），前端 81 passed，`pnpm build`，
  `./scripts/install.sh`，以及安装后 `--help` / `--show-last` 安全精确查询验证。

## 已确认的产品决策

1. `--show-last` 复用普通 history 并尊重历史开关。history limit 为 0 或条目已被裁剪时返回
   无结果，不另建绕过该设置的 last-exchange 存储。
2. Message 以 UTF-8 字节数计，超过 8 KiB 时改为私有文件；stdout 保留 2 KiB 前缀并输出
   全文绝对路径。questions 与 answers 仍完整输出。
3. 同时提供 CLI `AskHuman --show-last` 与 MCP `show_last`；CLI 完全非 Agent 调用时返回当前
   project 最后一条完成问答。
4. Codex 用 `_meta.threadId`；Claude/Cursor 的 `ask`、`whats_next`、`show_last` 全部统一走
   PreToolUse 注入的 schema-hidden one-time token，不再采用 PostToolUse result receipt。
5. Cursor 保留简洁工具名并接受同名 MCP 工具可能收到隐藏字段的低概率兼容风险；设置页推荐
   标签为 Cursor/Claude=CLI、Codex/Grok=MCP，但标签不进入提示词。用户实际选择 CLI 时所有
   运行时文案只写 CLI，实际选择 MCP 时只写 MCP；Cursor MCP 同名兼容问题发生后由用户在设置
   页切换模式，不在 MCP Rule 中预埋 CLI 替代入口。
6. 真实 Agent session 完全不可得时，MCP 用 `mcp_instance_id + project` best-effort；已取得真实
   session 但查不到记录时不向弱键级联。history 开关/裁剪仍优先。
7. Grok 0.2.106 的 PreToolUse updatedInput 与 MCP session `_meta` 均经真机否定；首版采用
   PreToolUse 参数指纹旁路，但把 `mcp_instance_id + project` 作为永久候选分区，只在候选恰好
   一条时认领真实 session，0/多条时退回 instance best-effort，绝不 FIFO/取最近。
8. 恢复 Hook 使用独立 marker/隐藏入口，由集成模式托管，不修改或依赖 experimental lifecycle
   tracking。其过期状态并入当前模式更新状态，不新增设置页独立行。Codex 恢复条目的 trust 必须
   与 hooks 配置原子 reconcile，失败时同时回滚 `hooks.json` 与 `config.toml`。

## 预计代码落点

| 文件/模块 | 主要变化 |
| --- | --- |
| `src-tauri/src/history.rs` | 新增两个兼容字段；增加 exact-session、MCP-instance+project、project 三种 latest 查询 |
| `src-tauri/src/ipc/mod.rs` | `TaskRequest` 增加内部 `mcp_instance_id`；增加 MCP instance 注册、Grok pending/claim 消息 |
| `src-tauri/src/daemon/request.rs` / 新 binding registry | 传递 history 归属；维护短 TTL 的 instance/process 关系与 Grok pending 唯一认领 |
| `src-tauri/src/app/coordinator.rs` | 构造 `HistoryEntry` 时写入两种归属键 |
| `src-tauri/src/cli/mod.rs` / help | 解析 `--show-last`，检测 Agent 与 project，调用统一查询核心 |
| `src-tauri/src/cli/output.rs` 或新模块 | 格式化完整 exchange；实现 8 KiB/2 KiB Message 文件策略 |
| `src-tauri/src/paths.rs` | 增加 show-last 私有正文路径与短命 token 目录 |
| `src-tauri/src/mcp/ask.rs` | `AskServer` 持有 `mcp_instance_id`；三种参数解析 hidden token；读取 Codex `Meta`；新增 `show_last` |
| `src-tauri/src/agents/context_recovery.rs`（新） | 独立恢复 Hook runtime：compact 模式提示、Claude/Cursor token 注入、Grok wrapper/hash 上报；不产生 lifecycle 事件 |
| `src-tauri/src/integrations/agent_context_recovery.rs`（新） | 用独立 marker 安装/检查/卸载模式所需 SessionStart/PreToolUse；保留用户和 lifecycle Hook；处理 Codex trust 与双文件回滚 |
| `src-tauri/src/integrations/agent_mode.rs` | 把恢复 Hook 纳入 CLI Hook/MCP artifact 的 reconcile 与 `needs_update`，None 只清理恢复 marker |
| `src-tauri/src/integrations/agent_lifecycle.rs` | 保持独立实验功能与原 `__agent-hook` 命令；仅复用通用 hook edit/trust 基础设施，不承担恢复能力 |
| `src-tauri/src/integrations/agent_rules.rs` / `src-tauri/src/prompts.rs` | 分别更新 CLI/MCP prompt source；运行时文案严格服从已选 variant，不使用设置页推荐值 |
| `src/views/settings/IntegrationTab.vue` / `useIntegration.ts` | 不新增恢复 Hook 行；CLI Hook/MCP 更新状态纳入内部恢复子产物，并修正 Codex CLI“无 Hook”展示 |

token store 建议单独放进小模块而不是塞进 reporter/MCP handler，统一负责创建、原子消费、TTL、
权限与过期清理。查询与格式化也应让 CLI/MCP 共用，避免两条路径逐渐产生不同输出语义。

## 推荐实施顺序

1. 给 `HistoryEntry`、`TaskRequest`、Coordinator 和所有 history 构造路径补
   `agent_session_id + mcp_instance_id`，增加旧 JSONL 兼容测试。
2. 抽出统一 `show_last` 查询/格式化核心，实现 CLI 精确查询、非 Agent project fallback、8 KiB
   Message 私有文件与 2 KiB 前缀。
3. 新增 MCP `show_last`；`AskServer` 创建并共享进程级 `mcp_instance_id`，三种 MCP 调用都把它
   传给 history。
4. Codex 三个 handler 读取 `_meta.threadId`，清除旧 session env 后传递权威 thread ID。
5. 实现私有 token store、独立恢复 Hook、Claude/Cursor PreToolUse hidden token 注入与三种
   handler 消费；保留 Cursor 输入结构 guard，并覆盖与 lifecycle deny Hook 共存的输出合并语义。
6. 对 Grok 明确跳过 token 注入；在独立恢复 Hook 中实现 MCP instance/process 注册、
   PreToolUse canonical hash
   pending 与唯一候选 claim。确保 handler 清除继承的其它 Agent session env，并验证
   instance+project 永久分区、歧义降级与 MCP 重启行为。
7. 更新独立 CLI/MCP 静态 Rule/Skill；在 `agent_mode` 中按目标模式安装恢复 Hook，Codex/Claude
   SessionStart 在运行时读取当前模式并只输出对应入口。把恢复状态并入当前 artifact update，
   并完成 Codex trust 原子 reconcile/回滚。
8. 回归测试：短/长/空 Message、多问题、多选/自由文本/附件、取消、连续压缩、history 关闭、
   history 裁剪、旧 JSONL、并行 session、`/clear`/resume/切换 conversation、MCP server 复用、
   token 过期/重放/hook 缺失、Claude schema 外字段、Cursor 严格同名 MCP 工具、MCP 重启与
   非 Agent CLI project fallback；Grok 回归 Build wrapper、Composer wrapper、唯一/零/多候选、
   process 解析失败、hook/claim 竞态，以及 hook 仍忽略 updatedInput、`tools/call` 仍无 session
   `_meta`；升级版本后若行为变化再简化旁路。

## 主要代码证据

Codex：

- `codex-rs/core/src/compact.rs`：本地摘要、真实 user message 收集、预算截断、history 替换。
- `codex-rs/core/src/compact_remote.rs`：远程压缩结果的 item 白名单与工具项丢弃。
- `codex-rs/core/src/compact_remote_v2.rs`：V2 retained messages 形态。
- `codex-rs/core/src/session/mod.rs`：压缩后排队 `SessionStartSource::Compact`。
- `codex-rs/core/src/hook_runtime.rs`：SessionStart hook input 与 additional context 注入。
- `codex-rs/core/src/mcp_tool_call.rs`：每次 MCP 请求写入 `_meta.threadId`。
- `codex-rs/core/src/unified_exec/process_manager.rs`：Shell 子进程注入 `CODEX_THREAD_ID`。

AskHuman：

- `src-tauri/src/agents/detect.rs`：四家 session ID 环境变量映射。
- `src-tauri/src/cli/mod.rs`：CLI 已探测 `agent_session_id` 并写入 `TaskRequest`。
- `src-tauri/src/ipc/mod.rs`：`TaskRequest.agent_session_id` 与 MCP env 可能过期的现有说明。
- `src-tauri/src/daemon/request.rs`：RequestEntry 已暂存 session ID，但未传入 history
  Coordinator。
- `src-tauri/src/app/coordinator.rs`：`record_history()` 当前只写 `agent_kind`。
- `src-tauri/src/history.rs`：当前 HistoryEntry 已有完整 Message/questions/answers，缺 session ID。
- `src-tauri/src/mcp/ask.rs`：MCP ask spawn CLI 子进程，当前未读取 per-request `_meta`。
- `src-tauri/src/agents/report.rs`：现有 Pre/PostToolUse reporter 已解析当前 session、工具名、输入
  和阶段，其解析/deny 输出逻辑可抽成共享 helper；恢复 runtime 复用 helper，但不复用 lifecycle
  marker 或上报生命周期事件。
- `src-tauri/src/integrations/agent_lifecycle.rs`：四家 lifecycle tracking 是独立实验开关，不能作为
  恢复能力前置条件；其 JSONC/Codex Hook 编辑代码可抽成通用基础设施。
- `src-tauri/src/integrations/agent_stop.rs`：现有共享 Stop handler 证明 capability 可协调管理同一
  配置文件；本方案最终选择独立恢复 marker，以保持旧 lifecycle 命令与干净降级兼容。
- `src-tauri/src/integrations/agent_permission.rs`：`reconcile_codex_trust()` 已按 matcher、command、
  timeout、status message 和数组位置计算 Codex canonical identity，可供恢复 Hook 复用；调用方仍
  需负责 hooks/config 双文件备份与失败回滚。

Claude/Cursor 已安装版本的静态证据：

- Claude binary `2.1.205`：搜索 `async function vgo` 可见最终
  `callTool({name:o,arguments:i,_meta:s})`；工具包装处只构造
  `{"claudecode/toolUseId": ...}`。搜索 stdio transport 的
  `CLAUDE_CODE_SESSION_ID:Ct()` 与 `/clear` 的 `process.env.CLAUDE_CODE_SESSION_ID=Ct()`
  可确认启动时注入和后续父进程换 ID；clear 路径只显式重连 `ide` MCP client。当前 changelog
  与本机 transcript 也证明 PreToolUse `updatedInput` 已被实际使用；schema 外 MCP 字段仍需专门
  实测。
- Cursor CLI `index.js`：`McpSdkClient.callTool()` 调用
  `this.client.callTool({name:t,arguments:n})`；`DefinitionMcpLoader.clientCache` 跨 load 复用
  client，`fromCommand(..., env)` 的 env 来自 MCP 配置。
- Cursor Desktop `cursor-agent-exec/dist/main.js`：Shell 执行器按每次执行的
  `t.conversationId` 注入 `CURSOR_CONVERSATION_ID`，说明 Shell `--show-last` 能得到当前 ID；
  该逐执行注入不在 MCP `tools/call` 路径中。其 MCP wrapper 还显示 PreToolUse 只得到
  `MCP:<toolName>` 并可合并 `updated_input`；带 `mcp_server_name` 的 beforeMCPExecution 则只
  消费 permission，不会再修改入参。PostToolUse 在结果返回模型前同步执行，并能看到完整
  `content` 序列化结果。
- Grok `0.2.106`：随附 `~/.grok/docs/user-guide/10-hooks.md` 的 PreToolUse 输出仅定义
  `decision=allow|deny`。2026-07-22 隔离真机探针确认普通工具与 stdio MCP 工具都忽略
  `updatedInput`；MCP hook input 有真实 `sessionId/toolUseId` 和限定工具名，但实际
  `tools/call._meta` 只有 `progressToken`，stdio server env 无 `GROK_SESSION_ID`。三个探针模型
  调用合计费用约 `$0.101`，临时项目与探针配置在验证后删除。
