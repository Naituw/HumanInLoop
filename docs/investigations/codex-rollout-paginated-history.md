# Codex 0.147+ rollout 格式变更：对 AskHuman 的影响

调查日期：2026-08-20。对照上游 `openai/codex` 的 `rust-v0.146.0` / `rust-v0.147.0` / `rust-v0.148.0`，以及本机 `~/.codex` 实盘。

## 结论

**不是「JSONL 行 schema 在 0.147 整体换了一套」。** 0.146 与 0.147 的 `RolloutItem` 线格式相同。真正的分叉是 **`history_mode: paginated`**：同样的 `~/.codex/sessions/**/rollout-*.jsonl` 路径下，权威用户/助手/补丁完成事件不再写 `event_msg/user_message` 等旧类型，改为 `event_msg/item_completed` + `TurnItem`。

对本仓库的含义：

| 面 | 当前默认（legacy JSONL） | 一旦会话变成 paginated |
|---|---|---|
| 权限记忆（`permission_memory` 扫 `turn_context` / `function_call`） | 无影响 | **基本无影响**（这两类仍落盘）；读不到文件则 fail-closed 为基础弹窗 |
| MCP Ctrl+C 取消（扫 `turn_aborted`） | 已按 0.147 适配 | **无影响**（paginated 仍持久化 `turn_aborted`） |
| 标题 / `/transcript` / show_last User Prompt | 无影响 | **主路径失效**，靠 `response_item` 回退，可能掺注入块或丢权威用户行 |
| Watch / 控制台活动足迹 | 无影响 | **丢 `patch_apply_end` 与 `agent_message`**；Code Mode 写文件足迹会缺 |
| 会话发现 / 工作区 cwd 扫描 | 无影响 | 只认 `*.jsonl`，**不认 `.jsonl.zst`**；`session_meta.cwd` 本身仍在 |

本机现状（2026-08-20）：CLI `codex-cli 0.145.0`；Desktop 会话 `cli_version=0.147.0-alpha.6.5`，`state_5.sqlite` 里 228 条 thread **全部 `history_mode=legacy`**，JSONL 仍是 `{timestamp,type,payload}`，含我们解析的全部旧事件。**当前这台机器没有被 paginated 打中。**

## 1. 0.147 到底改了什么

### 1.1 没有改的（线格式）

`codex-rs/protocol/src/protocol.rs` 的 `RolloutItem` 在 0.146 与 0.147 一致：

`session_meta` / `response_item` / `event_msg` / `turn_context` / `world_state` / `compacted` / 少量 inter-agent 元数据。

本机 Desktop 0.147-alpha 实盘抽样：

- 顶层键只有 `timestamp` + `type` + `payload`（尚未出现 `ordinal`）
- `event_msg/user_message.payload.message` 仍是字符串
- `turn_context.payload` 仍有 `turn_id` / `approval_policy` / `approvals_reviewer`
- `response_item` 仍有 `function_call` / `custom_tool_call`
- `event_msg/patch_apply_end` 仍在

`UserMessageEvent` / `AgentMessageEvent` / `TurnContextItem` 在 0.146–0.148 结构未改。

0.147 另有 **opt-in 诊断** `CODEX_ROLLOUT_TRACE_ROOT`（`codex-rs/rollout-trace`），写独立 bundle，**不是** session rollout，我们不读。

### 1.2 真正引入的：paginated history

0.147 起 rollout 按 `ThreadHistoryMode` 分两套持久化策略（`codex-rs/rollout/src/policy.rs`）：

- **`legacy`（enum 默认值）**：继续写 `event_msg/user_message`、`agent_message`、`patch_apply_end`、`mcp_tool_call_end` 等。`item_completed` 只保留 Plan / Sleep。
- **`paginated`**：上述 legacy 完成事件 **不再落盘**；改写 `event_msg` `type=item_completed`，payload 里是 `TurnItem`（`UserMessage` / `AgentMessage` / `FileChange` / …）。`response_item` 与 `turn_context` **两边都写**。

TUI 在 0.147 已对非 ephemeral 会话 **请求** `history_mode: Paginated`，但有回退：app-server 不支持 `thread/turns/list`、或判定 `LegacyOnly` 时改回 `None`（即 legacy）。本机 Desktop 走的是回退路径。

配套工具（0.147 合入、0.148 有 CLI）：

- `codex migrate-rollouts`：默认 dry-run；`--apply` 把 legacy JSONL **改写**成 paginated 规范 JSONL，并投影到 SQLite
- feature `background_paginated_rollout_migration`：**默认关**，stage = UnderDevelopment
- feature `local_thread_store_compression`：**默认关**；冷文件可压成 `rollout-*.jsonl.zst`

0.148 额外：

- `codex exec` 非 ephemeral：**默认 Paginated**（0.147 exec 还没有这条）
- `RolloutItem` 迁到 `codex-rs/history`；`response_item` 可带可选 sibling `metadata`；新类型 `security_risk_score`。多出来的键我们的宽松 JSON 解析会忽略。

## 2. paginated JSONL 长什么样

迁移/直播 paginated 的权威用户输入不再是：

```json
{"type":"event_msg","payload":{"type":"user_message","message":"帮我修 bug"}}
```

而是（示意，`TurnItem` 使用 `#[serde(tag = "type")]`、无 `rename_all`，故 PascalCase）：

```json
{
  "timestamp": "...",
  "ordinal": 12,
  "type": "event_msg",
  "payload": {
    "type": "item_completed",
    "thread_id": "...",
    "turn_id": "...",
    "item": {
      "type": "UserMessage",
      "id": "...",
      "content": [{ "text": "帮我修 bug", "text_elements": [] }]
    },
    "completed_at_ms": 0
  }
}
```

`patch_apply_end` 对应 `item.type = "FileChange"`（`changes` map 仍在）。`agent_message` 对应 `item.type = "AgentMessage"`。

我们现有匹配全部落空：

- `title.rs` / `transcript_full.rs`：`payload.type == "user_message"` 且 `payload.message` 为字符串
- `activity.rs`：`event_msg` + `agent_message` / `patch_apply_end`

`session_meta` 仍在文件头；paginated 会把 `history_mode` 写成 `"paginated"`，并可带 `ordinal`。

## 3. 我们各读取面

实现入口：

- 路径：`title.rs` `transcript_path` / `codex_title`，只找 `rollout-*-<sid>.jsonl`（深度 4），**不认 `.jsonl.zst`**
- 标题：先 `event_msg/user_message`，再 `response_item` 用户行（跳过 `<…>` 与 `# AGENTS.md instructions`）
- 完整会话：`transcript_full.rs` `push_codex`
- 活动足迹：`activity.rs` `push_events_codex`
- 工作区：`workspaces.rs` 扫 jsonl 的 `session_meta.payload.cwd`
- 权限：hook stdin 的 `transcript_path` → `scan_rollout`（`turn_context` + 当前 turn 的 `function_call`/`custom_tool_call`）。二进制/过大/缺文件 → `Unproven`，基础弹窗
- MCP 取消：`mcp/ask.rs` 从同一 jsonl 尾扫 `turn_aborted`（0.147 已加；paginated 仍写）

权限与取消走 hook/metadata 给的绝对路径，不依赖我们自己 glob。标题、transcript、watch、工作区依赖 glob `*.jsonl`。

## 4. 风险判断

**现在（多数用户仍是 legacy，后台迁移默认关）**：无功能性回归。本机 Desktop 0.147-alpha 已验证。

**即将变真的路径**：

1. 用户升级到 **Codex 0.148+ 并用 `codex exec` 持久会话**（默认 paginated）
2. 用户执行 `codex migrate-rollouts --apply`，或打开 `background_paginated_rollout_migration`
3. TUI/Desktop 在 thread-history SQLite 就绪后不再回退 legacy（本机尚无 thread-history DB）
4. 冷会话被压成 `.jsonl.zst`：标题/transcript/watch/工作区找不到文件；权限若 hook 仍给 `.jsonl` 而磁盘只剩 `.zst`，降级为基础弹窗

**不会炸权限记忆**：paginated 仍写 `turn_context` 与 `response_item` 工具调用。最坏是读文件失败 → 已设计的 fail-closed。

**会 silently 变差的产品面**：Agent 控制台标题、IM `/transcript`、watch 足迹、show_last 的 User Prompt（spec 把 `event_msg/user_message` 定为 Codex 权威用户行）。

## 5. 若要跟进，解析器最小补丁

不需要读 SQLite。继续读 JSONL 即可，但要：

1. 把 `event_msg/item_completed` 里的 `UserMessage` / `AgentMessage` / `FileChange` 映射到现有 `UserText` / `AssistantText` / Write 足迹
2. 保留 legacy `user_message` / `agent_message` / `patch_apply_end`（双轨）
3. glob 同时接受 `rollout-*-<sid>.jsonl` 与 `*.jsonl.zst`，读 `.zst` 时解压或跳过（权限已把非 UTF-8 当 Unproven）
4. `response_item` 用户行去重逻辑按「后面是否有权威用户事件」扩展到 `item_completed`

不必立刻抬 `VERIFIED_CODEX_VERSION_CEILING`：那是 shell 判定对拍，与 rollout 行格式无关。

## 6. 实机验证（2026-08-20）

把本机全局 Codex 从 `0.145.0`（npm）升到 **`0.148.0`** 后：

```
codex exec --skip-git-repo-check --sandbox read-only -C /tmp/codex-paginated-probe \
  'Reply with exactly the token ASKHUMAN_PAGINATED_PROBE_20260820 …'
```

`state_5.sqlite`：`history_mode=paginated`，`cli_version=0.148.0`，`source=exec`。
JSONL 16 行全部带 `ordinal`。**没有** `user_message` / `agent_message` / `patch_apply_end`。
权威用户行是 `event_msg/item_completed` + `item.type=UserMessage`（`content[].type=text`）；
助手行是 `AgentMessage`（`content[].type=Text`，PascalCase）；
同文件仍有 `response_item` 的 `role=user` / `role=assistant` 副本。

第二次 `workspace-write` 会话（`01a01ecc-4f91-7740-8354-494eea787be6`）成功 `apply_patch` 写出 `probe.txt`。落盘的是 `item_completed/FileChange`（`status: "completed"`，`changes` 仍是路径 map），**没有** `patch_apply_end`。`custom_tool_call` / `function_call` 仍在。

对 AskHuman 现有解析器（`cargo test --bin AskHuman paginated_`）：

| 面 | 结果 |
|---|---|
| 标题主路径 `event_msg/user_message` | 空（符合预期） |
| 标题回退 `response_item` 用户行 | **仍能取到**真实 prompt（80 字截断照常） |
| `/transcript` / show_last User Prompt | **仍能取到** user + assistant（走 `response_item` 副本） |
| 纯问答的 watch 最后一段文字 | **仍能取到**（`response_item` assistant） |
| 写文件足迹 | **丢了**：`FileChange` 完全不可见；activity 只剩后续 `exec` Run 步，没有 Write |
| `turn_context` | 仍在，权限记忆扫描路径未破 |

2026-08-20 已在 `activity.rs` / `transcript_full.rs` / `title.rs` 补双轨解析：
legacy `user_message` / `agent_message` / `patch_apply_end` 与 paginated `item_completed`
`UserMessage` / `AgentMessage` / `FileChange` 并存，并对 `response_item` 副本去重。

## 7. 来源

- 上游：`policy.rs` `should_persist_event_msg`；`thread-store/.../rollout_migration/canonicalizer.rs`、`legacy_event.rs`；0.148 `exec/src/lib.rs` 与 0.147 `tui/src/app_server_session.rs` 的 `history_mode: Paginated`
- 本机：`~/.codex/state_5.sqlite` `threads.history_mode`；Desktop 0.147-alpha 仍是 legacy `sessions/2026/08/18/rollout-*-01a01574-….jsonl`；0.148 exec paginated `sessions/2026/08/20/rollout-*-01a01ec8-….jsonl` 与 `01a01ecc-….jsonl`
