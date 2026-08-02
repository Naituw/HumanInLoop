# 开发计划：show_last 多条 + 输出格式重设计

> 关联需求：`docs/specs/show-last-multi.md`  
> 计划描述方案与落点；具体代码以实现为准。

## 0. 方案总览

```
CLI:  AskHuman --show-last [N]
MCP:  show_last({ count?: 1..=10 })
        │
        ▼
  validate count ∈ 1..=10 (default 1)
        │
        ▼
  history::recent_sends_*(scope, n)     // 最近 n 条 Send
        │
        ├─ optional: last timestamped UserText from transcript(session)
        │
        ▼
  merge timeline, sort ascending (oldest first)
        │
        ├─ if any exchange answered_at > prompt.said_at
        │     prepend priority note
        │
        ▼
  render indented dialogue script → stdout / MCP text
```

**不改** `prompts.rs` / 已安装 Rule / compact 注入正文。

---

## 1. `history.rs`

```rust
pub fn recent_sends_for_session(agent_kind: &str, session_id: &str, n: usize) -> Vec<HistoryEntry>;
pub fn recent_sends_for_mcp_instance(mcp_instance_id: &str, project: &str, n: usize) -> Vec<HistoryEntry>;
pub fn recent_sends_for_project(project: &str, n: usize) -> Vec<HistoryEntry>;
```

- 过滤 `Send`，`timestamp_ms` **降序** `take(n)`（选取最近 N）。
- `latest_send_*` 委托 `recent_*(…, 1)`。
- 单测：分区、Cancel 排除、排序、n 大于库存。

---

## 2. User Prompt 读取

新增薄 API（建议 `agents/transcript_full.rs` 或 `show_last` 旁路模块）：

```rust
pub struct LastUserPrompt {
    pub text: String,
    pub at_ms: i64,  // 必须有；来自 UserText.at * 1000
}

pub fn last_timestamped_user_prompt(kind: AgentKind, session_id: &str) -> Option<LastUserPrompt>;
```

- `load_events` 后从新到旧找第一条非空真实 `UserText`。
- 时间：优先 `UserText.at`；若空，**best-effort** 解析 Cursor 风格 `at_label` / 正文 `<timestamp>…</timestamp>`（spec D9b）→ unix；仍失败 → 整条丢弃。
- Cursor IDE：`load_events` 已优先 vscdb，`bubble.createdAt` 通常有值。
- 复用 `clean_user`；失败全部 `None`；**永不**让 show_last 因 transcript 失败。
- session 线索：`recover` 增加 `Option<TranscriptHint { kind, session_id }>`，与 history scope 解耦。
---

## 3. `show_last.rs`

### 3.1 API

```rust
pub fn recover(scope: &Scope, count: usize, transcript: Option<TranscriptHint>) -> Result<String, Error>;
```

### 3.2 时间线模型

```rust
enum TimelineItem {
    Exchange { index: usize /* 输出前再编号 */, entry: HistoryEntry },
    UserPrompt(LastUserPrompt),
}
```

1. 一次扫描得到 `total`（scope 内 Send 总数）与最近 `n` 条 → `shown`。  
2. 可选 push UserPrompt。  
3. **升序** sort by time。  
4. 对 Exchange 按输出顺序重编号 1..shown。  
5. 若存在 Prompt 且存在更晚 exchange → Prompt section 末尾（上方空一行）写 `priority note`。  
6. 外壳：见 spec §3.3；传入 `Surface::Cli|Mcp`。`shown < total` 时：  
   - CLI：`… use --show-last [N] for more`  
   - MCP：`… use count=[N] for more`  
7. User Prompt 横幅永不带序号。

### 3.3 渲染细节

- 时间格式化：相对英文 + 本地绝对；测试注入 `now_ms`。  
- `says` / `user says` 长文本：8KiB/2KiB + 块内 `full message:`；文件名 hash 含 entry id 或 `prompt` 专用 key。  
- `user answered` 选项：`", "` 单行连接。  
- 空答：`user did not answer` 整行。

### 3.4 单测

- count=1 / >1 外壳。  
- 正序混排 + 编号。  
- priority note 开/关。  
- 无 at 的 prompt 不出现。  
- 长文本路径不互盖。  
- 替换全部旧 tag 断言。

---

## 4. CLI

- 解析 `--show-last [N]`；非法 exit 1。  
- `help` / `agent-help` 文案补 N。  
- `TranscriptHint` 来自 `caller_context` 的 kind + session_id（有则 Some）。

---

## 5. `permission_memory.rs`

```rust
Some("--show-last") => {
    args.len() == 1
        || (args.len() == 2 && is_decimal_1_to_10(&args[1]))
}
```

单测覆盖合法/非法形状。

---

## 6. MCP

- `ShowLastParams.count: Option<u32>`。  
- 校验 1..=10。  
- description 更新（tools/list 可见；**不是**改安装 Rule）。  
- binding fingerprint：公开参数含 count 时纳入 canonical hash（对照现网 show_last 空对象指纹）。  
- `TranscriptHint` 来自 resolve_binding 的 session。

---

## 7. 明确不做

- **不修改** `prompts.rs` 中 Rule/MCP skill/compact 字符串。  
- 不改 history UI。  
- 不写 asked_at 字段。

---

## 8. 实现步骤

1. history `recent_sends_*` + 测。  
2. `last_timestamped_user_prompt` + 测（可用合成 jsonl fixture）。  
3. show_last 时间线 + priority note + 新 render + 测。  
4. CLI / help / permission 白名单。  
5. MCP count + fingerprint。  
6. `./scripts/install.sh` 手工：无参、count=3、非法 N、有 session 时是否出现 Prompt/note（本机有真实 transcript 时）。  
7. 实现微调时回写 spec 样例。

---

## 9. 风险

| 风险 | 处理 |
| --- | --- |
| Breaking 输出 | 有意；无兼容层 |
| transcript 2MiB 读 | show_last 低频可接受；可后续 tail 优化 |
| Cursor jsonl 仅有英文 label | 解析 label；失败则不加 Prompt（IDE vscdb 通常已有 at） |
| note 被模型忽略 | Prompt section 末尾短句（贴在更新 AskHuman 之上）；不改系统提示 || fingerprint 漏 count | 联调 Grok/Claude token |

---

## 10. 预计落点

| 文件 | 变化 |
| --- | --- |
| `src-tauri/src/history.rs` | recent_sends |
| `src-tauri/src/show_last.rs` | 主逻辑 |
| `src-tauri/src/agents/transcript_full.rs` | last user prompt helper |
| `src-tauri/src/cli/mod.rs` / `help.rs` | N + 文案 |
| `src-tauri/src/mcp/ask.rs` | count |
| `src-tauri/src/permission_memory.rs` | 白名单 |
| `src-tauri/src/context_binding.rs` | 若需 fingerprint |
| `docs/specs/show-last-multi.md` | 已定案 |
