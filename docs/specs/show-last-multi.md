# show_last 多条恢复 + 输出格式重设计

> 状态：设计定案（2026-08-01，经 AskHuman 多轮评审），待实现。  
> 实现分支：`feat/show-last-multi`（worktree `../HumanInLoop-show-last-multi`，Dev Instance popup-only）。  
> 关联：`docs/plans/agent-context-compaction-retention.md`（既有单条 show_last 背景）、`docs/specs/reply-history.md`、`docs/specs/mcp.md`。  
> 问题背景：用户提供的 `show-last-recovery-priority.md`（压缩后系统提示「以最后 User Prompt 为准」与 show_last 恢复结果冲突）。

## 1. 需求

上下文压缩或会话过长后，agent 需要恢复自己与人类通过 AskHuman 完成的问答。现状：

1. **只返回 1 条**最近完成 exchange，不足以覆盖「最近几轮决策」。
2. **输出扁平 tag**（`[message]` / `[question]` / `[answer_*]`）表达不了层级；多题时 agent 难以区分哪段是自己发的、哪段是用户答的。
3. **没有时间信息**，无法判断「多久前被回答」。
4. 即使恢复了正确的 AskHuman，部分 agent 的系统提示仍写「以最后一次 User Prompt 为准」，会**压过**时间上更晚的 AskHuman 结论（见 §1.1）。

本需求：

- CLI / MCP 支持按 **count** 取最近 N 条已完成问答（默认仍为 1）；
- **重做 stdout 契约**：缩进对话稿，角色明确，多题 Q&A 成对绑定；
- 每条 exchange 标注**被回答**的相对时间 + 本地绝对时间；
- 在能解析到 session 时，**best-effort** 并入当前会话**最近一条带时间戳的真实 User Prompt**，与 exchange **按时间正序**混排，便于判断谁更新；
- 当存在「晚于该 User Prompt 的 AskHuman」时，输出**优先级提示**，避免 agent 仍死守系统提示里的 User Prompt 规则。

### 1.1 优先级误判背景（来自现场分析）

系统侧常同时有：

1. 压缩后仍可见旧对话，并要求「把最后一个 User Prompt 视为当前请求」；
2. 压缩后必须 `show_last` 恢复 AskHuman。

若压缩前残留了**别的任务**的 User Prompt，而其后 AskHuman 才是真实当前任务，则：`show_last` 数据正确，但 agent 仍按规则 1 跑偏。  
根因是恢复结果**没有**声明「后置 AskHuman 应成为新的任务基线」。本 spec 用时间线 + 条件 `priority note` 补上。

## 2. 非目标

- 不新增旁路存储；仍以 `history.jsonl` 为唯一 AskHuman 数据源，并尊重 `history_limit`。
- 不记录 / 不**展示**「提问发出时间」；AskHuman 排序与展示只用 `HistoryEntry.timestamp_ms`（回答落盘）。
- 不改变 `ask` / `whats_next` 的返回区块格式。
- 不扩展 scope 的权威性规则（精确 session 不向弱键级联查 history）。
- **不修改** CLI/MCP Rule、compact 短提示或其它 agent 安装提示词；默认仍鼓励无参调用。
- 本轮不更新主 `docs/overview.md`（实现完成后再补能力描述）。

## 3. 设计定案

### D1 API：默认 1 条，显式 count 才多条

| 入口 | 形态 | 默认 | 上限 |
| --- | --- | --- | --- |
| CLI | `AskHuman --show-last [N]` | 省略 N ⇒ **1** | **10** |
| MCP | 公开参数 **`count`**（integer，optional） | 省略 / null ⇒ **1** | **10** |

- **N / count 必须为 1..=10 的整数**。非法：CLI stderr + exit 1；MCP `isError`。  
  文案例：`count must be an integer between 1 and 10`。
- MCP 参数名必须是 **`count`**（不要 `limit`）。
- CLI：`--show-last` 后至多一个十进制整数；不得再有其它 argv。
- 权限记忆自调用白名单放行：裸 `--show-last`，以及 `--show-last` + `1..=10` 的单一数字。
- **不改提示词**；agent 默认无参即可。count 仅高级可选。

### D2 查询语义（AskHuman 条）

- 过滤：`action == Send`。
- 分区 scope 与现网一致（`show_last::Scope`）；**history 不向弱键级联**。
- **选取**：按 `timestamp_ms` 降序取前 `min(count, 可用条数)`（最近 N 条）。
- **展示顺序**：见 D8（正序时间线）。
- `history_limit == 0` → history disabled；0 条 → NotFound。

### D3 输出格式：缩进对话稿（breaking）

旧 `[message]` / `[question]` / `[answer_*]` **不再兼容**。

#### 3.1 角色与键名（全小写）

| 键 / 行 | 含义 |
| --- | --- |
| `answered at:` | AskHuman exchange 被回答时间 |
| `said at:` | User Prompt 发出时间（transcript） |
| `assistant (you) says:` | Agent 的 shared message |
| `assistant (you) asked:` | 某一题题干 |
| `user answered:` | 用户对该题的答题字段 |
| `user did not answer` | 该题无有效回答时的整行占位 |
| `user says:` | 会话里的 User Prompt 正文 |
| `qa #n:` | 第 n 题（单题也 `#1`） |
| `selected_options:` / `user_input:` / `files:` | 仅在 `user answered:` 下 |
| `full message:` / `files:` | 仅在 `assistant (you) says:` 或 `user says:` **块内部** |
| `priority note:` | 条件出现的任务优先级说明（D9） |

角色：**assistant (you)** = 调用方 agent；**user** = 人类。

#### 3.2 时间行

```text
answered at: 2 minutes ago (2026-08-01 14:32:05 +0800)
said at: 15 minutes ago (2026-08-01 14:19:01 +0800)
```

- 相对：固定英文。
- 绝对：本地时区 `YYYY-MM-DD HH:MM:SS ±HHMM`。
- AskHuman 数据源：`timestamp_ms`（回答落盘）。不存 / 不展示 asked_at。

#### 3.3 总头（header）、稳定编号与 Prompt 后省略

查询时取出本次返回的最近 `shown` 条，并得到 scope 内 **Send 总数** `total`（用于稳定绝对编号）。有 User Prompt 时另计 `after_prompt` = `answered_at > prompt.said_at` 的 Send 数。

**何时写总头 + 每条 Exchange 横幅**

| 条件 | 写总头 + `Exchange #k` 横幅 |
| --- | --- |
| `shown == 1` 且 **无** User Prompt 且 `total == 1` | 否（极简单条） |
| `shown == 1` 且 **无** User Prompt 但 `total > 1` | **是** |
| `shown == 1` 且 **有** User Prompt | **是**（与 Prompt 分节） |
| `shown >= 2` | **是** |

**总头（刻意简单，无选取歧义）**

- 单条：`show_last: 1 exchange`
- 多条：`show_last: 3 exchanges`
- **不写** `(oldest first)`（易被理解成「从 session 最早开始取」；实际是取最近 N 条再按时间正序打印）。
- **不写** `of total` / more；「还有多少」改由 Prompt 后省略行承担（有 Prompt 且有省略时）。

**Exchange 稳定绝对编号**

- 全 scope 按回答时间升序：最旧 `#1`，最新 `#total`。
- 本次只展示最近 N 条时，横幅仍用绝对号（例 total=74、count=1 → `Exchange #74`；count=3 → `#72` `#73` `#74`）。
- 不同 N 多次调用时，同一条的编号不变。

**输出顺序**：仍按时间升序（旧 → 新）打印时间线条目；靠绝对号 + `answered at` 自解释，不靠 oldest first 文案。

**User Prompt 后省略行**（`omitted = after_prompt − shown_after_prompt`，仅 `omitted > 0`）

位置：User Prompt section 内，`priority note` **之后**（无 note 则在 `user says` 后），**下一条 Exchange 横幅之前**：

CLI：

```text
… 70 AskHuman exchanges omitted after this prompt; use --show-last [N] for more …
```

MCP：

```text
… 70 AskHuman exchanges omitted after this prompt; use count=[N] for more …
```

- 一律 `exchanges`（含 N=1）。
- 只统计 **晚于该 Prompt** 的 AskHuman；Prompt 之前的轮次不在此行表达。
- User Prompt 横幅：`━━━━━━━━ User Prompt ━━━━━━━━`（无序号，不计入 shown）。

#### 3.5 多题配对

```
exchange
├── answered at
├── assistant (you) says? 
└── qa #1..m
    ├── assistant (you) asked
    └── user answered | user did not answer
```

禁止把全部 question / answer 拆成两大段。缩进：块内正文 2 空格；`files` 列表再 2 空格 + `- `。

### D4 省略规则

- 无 shared message 文本且无 message 附件：省略 `assistant (you) says:`。
- `user answered` 内空键整键省略。
- 不回放未选项 / recommended。

### D5 空回答

```text
qa #2:
  assistant (you) asked:
    需要 IM 吗？
  user did not answer
```

禁止 `user answered:` 嵌套包装该句。

### D6 长正文与附件：单一 `says` 块

`assistant (you) says` 与 User Prompt 的 `user says` 正文均使用 **512 B UTF-8 总预览预算**：

- 未超过 512 B 时原样输出；
- 超过时约各用一半预算保留头部与尾部，优先收缩到附近段落、换行或句子边界，中间输出
  `… [middle omitted] …`；
- 同时把未截断全文写入 0600 私有文件。`full message:` 与 `files:` 写在**同一** `says` 块内；
- 问题、用户实际回答、已选项与附件路径不使用该正文预算，继续完整恢复。

```text
assistant (you) says:
  <约 256 B 头部>
  … [middle omitted] …
  <约 256 B 尾部>
  full message: /…/show-last/<hash>.md
  files:
    - /path/context.pdf
```

全文文件 hash 须含 entry id（或多条不互盖）；权限 0600。

### D7 User Prompt 并入（best-effort）

**目的**：让 agent 看到「最近真实用户原话」与 AskHuman 的先后，从而判断任务优先级。

#### 7.1 何时尝试

- 调用上下文中**有任何可用的 `session_id` 线索**（含 AgentSession 精确绑定；以及 binding 解析到的 session，即使 history 落在 McpInstance 分区——history 与 transcript 查找独立）。
- 需要 `agent_kind`（或可映射到 `AgentKind`）以便 `transcript_full::load_events` / 等价路径。
- 失败（无 kind、无文件、解析失败、无真实 user 文本）：**静默不加**，不影响 AskHuman 输出。

#### 7.2 取哪一条

- 扫 transcript 事件，取**时间上最近**的一条真实用户输入：  
  复用 `clean_user` / 注入块过滤（跳过 AGENTS.md、environment_context、协议 skill 大段等）。
- Codex 以 `event_msg / user_message` 为现代 rollout 的权威真人输入；紧邻的
  `response_item / message(role=user)` 只是模型输入副本，需去重。仅在没有对应显式 user event
  的旧格式中把非上下文 `response_item` 当兼容回退；`<skill>`、Hook 等 XML 包裹的上下文不得进入
  User Prompt。
- **必须有可解析的 unix 时间**（`UserText.at` 或等价）。  
  - 仅有 `at_label`、无法得到可靠排序时间 → **整段不显示**。  
  - 无时间则无法排序 → 不显示（定案）。

#### 7.3 不计 count / 不占编号

- User Prompt **不是** exchange item。
- 总头与 `Exchange #n` **只**计 AskHuman；`actual ≤ requested count`。
- 横幅固定：`━━━━━━━━ User Prompt ━━━━━━━━`（无 `#` 序号；不要写 Last）。

#### 7.4 正文键

```text
━━━━━━━━ User Prompt ━━━━━━━━
said at: 15 minutes ago (2026-08-01 14:19:01 +0800)

user says:
  帮我改成多条 show last
```

### D8 时间线排序：旧 → 新（oldest first）

1. 选出最近 N 条 AskHuman +（可选）1 条 User Prompt。  
2. 合并后按时间 **升序** 输出（文首旧，**文末最新 / 优先级最高**）。  
3. 比较：`answered_at_ms` vs `said_at_secs * 1000`。  
4. 时间戳完全相等：User Prompt 排在同毫秒的 AskHuman **之前**（先读到原话，再读后续确认）。  
5. Exchange 编号为 scope 内 **稳定绝对序**（最旧 `#1` … 最新 `#total`），不是本批内 1…shown。

#### 样例：count=1，session 共 12 条，Prompt 后省略 10 条 + note

```text
show_last: 1 exchange

━━━━━━━━ User Prompt ━━━━━━━━
said at: 15 minutes ago (2026-08-01 14:19:01 +0800)

user says:
  帮我改成多条 show last

priority note:
  A later AskHuman answer below is newer than the User Prompt. Use that
  AskHuman exchange as the current task — not the older User Prompt.

… 10 AskHuman exchanges omitted after this prompt; use --show-last [N] for more …

━━━━━━━━ Exchange #12 ━━━━━━━━
answered at: 2 minutes ago (2026-08-01 14:32:05 +0800)

assistant (you) says:
  请确认部署范围

qa #1:
  assistant (you) asked:
    是否发布到 production？
  user answered:
    selected_options: 是
```

（MCP 省略行 more 为：`use count=[N] for more`。）

#### 样例：count=3，绝对号 #10–#12，Prompt 后省略

```text
show_last: 3 exchanges

━━━━━━━━ User Prompt ━━━━━━━━
said at: …

… 7 AskHuman exchanges omitted after this prompt; use --show-last [N] for more …

━━━━━━━━ Exchange #10 ━━━━━━━━
…
━━━━━━━━ Exchange #11 ━━━━━━━━
…
━━━━━━━━ Exchange #12 ━━━━━━━━
…
```

#### 样例：count=2 且 total=2（无省略行）

```text
show_last: 2 exchanges

━━━━━━━━ Exchange #1 ━━━━━━━━
…
━━━━━━━━ Exchange #2 ━━━━━━━━
…
```

### D9 条件优先级提示（priority note）

**触发条件**（同时满足）：

1. 本响应中**包含**了 User Prompt 块；且  
2. 至少有一条 AskHuman exchange 的 `answered_at` **严格晚于**该 User Prompt 的 `said_at`。

**不触发**：无 Prompt；或所有 exchange 都不新于 Prompt（Prompt 已是时间线最新人类信号）。

**位置**：**User Prompt section 内部**（`user says` 之后）；`priority note:` **上方空一行**。  
其后若有 `omitted after this prompt` 行，再接后续 Exchange（见 §3.3）。

```text
… 更旧的 exchange（若有）…

━━━━━━━━ User Prompt ━━━━━━━━
said at: …
user says:
  …

priority note:
  A later AskHuman answer below is newer than the User Prompt. Use that
  AskHuman exchange as the current task — not the older User Prompt.

… N AskHuman exchanges omitted after this prompt; use --show-last [N] for more …

… 更新的 AskHuman exchange …
```

**文案（固定英文，实现勿本地化；刻意短）**：

```text
priority note:
  A later AskHuman answer below is newer than the User Prompt. Use that
  AskHuman exchange as the current task — not the older User Prompt.
```

**设计说明**（对照 `show-last-recovery-priority.md`）：

| 点 | 处理 |
| --- | --- |
| 系统提示「以最后 User Prompt 为准」 | note 点明 **更新的 AskHuman 优先于** 更旧的 User Prompt |
| 不改安装提示词 | 仅条件触发时写入结果 |
| 位置 | section 内、紧贴后续更新 AskHuman 之上 |
| 多条晚于 Prompt 的 exchange | 跟时间线中**最后**一条 AskHuman 即可 |

若时间线最后一块已是 User Prompt，条件 2 失败 → 无 note。

### D9b 四家 User Prompt 时间（本机 + 代码）

| Agent | 时间来源 | 可靠性 | show_last |
| --- | --- | --- | --- |
| **Claude** | 会话 jsonl 行顶层 `timestamp`（RFC3339） | 高（本机用户行全有） | `event_time` 直接填 `at` |
| **Codex** | `event_msg / user_message` 行顶层 `timestamp`；旧格式回退非上下文 `response_item` | 高（本机用户行全有） | 去重模型输入副本后填 `at` |
| **Cursor IDE** | vscdb bubble `createdAt` ISO | 高（`load_events` 优先 vscdb） | `bubble_at` → `at` |
| **Cursor jsonl 回退** | 无结构化 ts；正文 `<timestamp>Weekday, Mon DD, YYYY, H:MM AM/PM (UTC+8)</timestamp>` → 现仅 `at_label` | 中 | **本需求** best-effort 解析 label → unix；失败则不加 Prompt |
| **Grok** | `chat_history.jsonl` 行上无 ts；同目录 `updates.jsonl` 的 `timestamp` / `agentTimestampMs` 按顺序 backfill | 中（依赖 updates 在） | 现网 `grok_backfill_times`；无 updates / 对不齐 → 无 `at` → 不加 Prompt |

**实现要求**：

1. 统一走 `transcript_full::load_events`（已含 Cursor vscdb 优先、Grok backfill）。  
2. 取最近真实 UserText；`at` 为空时再尝试解析 Cursor 风格 `at_label`。  
3. **禁止**用文件 mtime / composer `lastUpdatedAt` 冒充单条 Prompt 时间。### D10 MCP / help（仍不改 Rule 正文）

- `tools/list`：`show_last` description 可说明可选 `count` 与恢复载荷形态；**不**改 agents 安装的 Rule/Skill 文件内容。
- CLI `--help` / `--agent-help`：可选 N、默认 1、上限 10（帮助页，非注入 Rule）。

### D11 模块边界

| 模块 | 变化 |
| --- | --- |
| `history.rs` | `recent_sends_*` |
| `show_last.rs` | count、时间线 render、priority note、User Prompt 编排 |
| `agents/transcript_full`（或薄封装） | 取「最近带时间戳的真实 UserText」 |
| `cli/mod.rs` + `help.rs` | 解析 N；help |
| `mcp/ask.rs` | `count`；description |
| `permission_memory.rs` | 白名单 |
| `prompts.rs` / 已安装 Rule | **不改** |
| history UI | 无 |

## 4. 验收标准

1. 默认无参只**请求** 1 条 AskHuman；无总头、无 Exchange 横幅。  
2. `count=3` 历史充足 → 3 条 exchange，总头 `oldest first`，`#1` 最旧。  
3. 请求 5 仅 2 条 → 总头 `2 exchanges`。  
4. 非法 count → CLI exit 1 / MCP isError。  
5. 多题成对；空答 `user did not answer`。  
6. `answered at` / `said at` 格式正确；无 asked 展示。  
7. 有 session + 带时间 User Prompt → 按正序插入；**不计**总头条数。  
8. 无时间 / 无 transcript → 不加 Prompt，不失败。  
9. 存在晚于 Prompt 的 AskHuman → 顶部 `priority note`；否则无 note。  
10. 长 says / 长 user says：单块前缀 + `full message:`。  
11. 白名单：`--show-last` 与 `--show-last 5` 自调用放行。  
12. **提示词 / Rule 文件内容与现网一致**（本需求 diff 不得改 `prompts.rs` 注入正文，除非仅测疏漏——定案为不改）。  
13. 单测覆盖：渲染、排序、note 触发/不触发、count、分区、长文本路径不互盖。

## 5. 已否决方案（摘要）

- 扁平方括号；大段 Agent/Human 分 section；Markdown 标题风；JSON/XML 首选。  
- 默认多条；展示提问发出时间；MCP 名 `limit`；上限 20/50。  
- 空回答嵌套在 `user answered:` 下。  
- 长 message 拆多个顶层 section。  
- User Prompt 计入 item / 占 Exchange 编号 / 写 Last User Prompt。  
- 无时间仍展示 Prompt。  
- 时间线 newest-first（改为 oldest-first）。  
- 修改 agent 安装提示词来解决优先级（改为结果内条件 note）。  
- 为排序单独持久化 asked_at。
