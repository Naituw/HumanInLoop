# 重复提问收敛 —— 开发计划

> spec: `docs/specs/duplicate-ask-coalescing.md`（D1–D11）
> 分支 `feat/ask-coalesce`，worktree `../HumanInLoop-ask-coalesce`，Dev Instance popup-only。

## 概述

全部改动落在 **daemon 内**，CLI、前端、IPC 消息类型、输出契约都不动。两条新路径插在
`handle_submit` 的最前面：

```
CLI Submit
   │
   ├─ 同会话 + 同指纹 + 在途且未收尾  ──▶ 合流：挂成 follower，等同一份结果
   │
   ├─ 同会话 + 同指纹 + 5 分钟内答过  ──▶ 重放：直接回 Final（带 replayed 标注）
   │
   └─ 其余                            ──▶ 现有路径（create → 弹窗 / IM → 等结果）
```

会话键与指纹的口径见 spec D1/D2；两条路径都要求会话键存在，否则一律走现有路径。

## 阶段 1：指纹、会话键与重放缓存（纯逻辑，可独立单测）

新增 `src-tauri/src/daemon/ask_dedup.rs`：

- `session_key(task: &TaskRequest) -> Option<String>`：`agent_session_id` 非空优先，
  否则 `mcp_instance_id`，都空返回 `None`。两类键加前缀区分（如 `sid:` / `mcp:`），
  避免不同来源的字符串意外相等。
- `fingerprint(task: &TaskRequest) -> String`：按 spec D2 列出的字段依次喂进 sha2
  （已在依赖里），字段之间用不可能出现在内容里的分隔符隔开，文本先 `trim()`。
  选项要带上 `recommended` 与 `todoId`，附件按传入顺序取绝对路径。
- `ReplayCache`：`HashMap<会话键, ReplayEntry>` + 访问顺序队列，`ReplayEntry` 为
  `{ fingerprint, outcome: RenderOutcome, request_id, finished_at: Instant }`。
  - `put(session_key, entry)`：每会话覆盖式只留一条；超过 64 会话按 LRU 淘汰。
  - `get_fresh(session_key, fingerprint, now)`：指纹一致且 5 分钟内才命中，命中时
    顺带返回「已过去多少秒」（供 D7 文案）；过期项顺手清掉。
  - 窗口与容量作为模块常量。

`RenderOutcome`（`app/mod.rs`）加 `#[derive(Clone)]`——重放与合流广播都要复制它。

## 阶段 2：请求登记表支持多等待者

`src-tauri/src/daemon/request.rs`：

- `RequestEntry` 增加：
  - `session_key: Option<String>` 与 `fingerprint: String`（`create` 时算好存下，
    避免每次查找重算）;
  - `waiters: AtomicUsize`（`create` 时为 1）;
  - `followers: Mutex<Vec<UnboundedSender<RenderOutcome>>>`。
- 新增 `try_attach(session_key, fingerprint) -> Option<(Arc<RequestEntry>, UnboundedReceiver<RenderOutcome>)>`：
  - 在**同一把 `inner` 锁**内遍历 `by_id`（在途请求个位数，不建二级索引），匹配会话键
    与指纹；
  - 命中后检查 `coordinator.is_finalizing()`，已收尾的视为不在途、不挂（spec D9）；
  - 建一条 unbounded channel，发送端塞进 `followers`，`waiters += 1`，返回接收端。
- 新增 `release_waiter(&entry) -> usize`：`waiters -= 1` 并返回剩余数，供 EOF 判定。
- 新增 `broadcast_outcome(&entry, &RenderOutcome)`：把结果 clone 给所有 follower 发送端
  并清空列表。

`Coordinator`（`app/coordinator.rs`）增加 `winner_action() -> Option<ChannelAction>`：
`submit()` 里把首个终态的 action 存进一个 `Mutex<Option<_>>`（与已有 `winner` 同处），
供阶段 4 判定「是不是真实回答」（spec D6）。

## 阶段 3：合流路径

`src-tauri/src/daemon/runtime/mod.rs`：

- `handle_submit` 在排空闸门与 agent 活动刷新之后、`registry.create()` 之前：算出会话键
  与指纹；会话键为 `None` 直接走原路径。
- 命中 `try_attach` → 转入新函数 `handle_attached`：
  1. 写 `ServerMsg::Accepted { request_id: <被复用的 id> }`；
  2. `tokio::select!` 等 follower 的接收端 / 本连接 EOF；
  3. 收到结果 → 按现有格式写 `Warn`（若有）与 `Final`，然后 `release_waiter` 后返回；
  4. EOF → 只 `release_waiter`，**不**取消请求、不动卡片、不碰托盘与 watch；
  5. 日志：`request <id> coalesced (waiters=N)`。
  - 这条路径不 fire `ask-received`、不 `dispatch_popup`、不 `attach_im_channels`、
    不 `broadcast_tray_state`、不 `spawn_agent_resolve`（agent 活动刷新在函数更早处已做，
    保持原样即可）。
- 主路径的 EOF 分支（现在无条件 `cancel_request`）改为：先 `release_waiter`，**仍有等待者**
  时不取消，继续 `final_rx.recv()` 等结果（拿到后只广播给 follower，自己不写 IPC）；
  归零时才走原有取消收尾。
- 主路径拿到 outcome 后，在写自己的 `Final` 之前先 `broadcast_outcome`，保证 follower
  不因主连接后续收尾而延迟。

## 阶段 4：重放路径

- `ServerState` 增加 `replay: Mutex<ReplayCache>`。
- 未命中在途合流时，查 `get_fresh`：命中则写 `Accepted { request_id: <原请求 id> }` +
  `Final`（stdout 经下面的标注函数处理，退出码原样），记一条
  `request <id> replayed (Ns ago)` 日志后返回。这条路径不创建请求、不写历史、不触发
  hook、不动托盘与 watch。
- 写入缓存：主路径拿到 outcome 且 `winner_action() == Send` 时 `put` 一条；取消 /
  无渠道 / 看门狗失败等一律不写（spec D6）。
- 标注函数（放 `ask_dedup.rs`），载体复用 `status`（spec D7）：
  - 文本：在 stdout 最前插入 `[status]` 区块（用 `output::MARKER_STATUS`）+ 一行本地化
    说明 + 空行；
  - JSON（`output_format == Json`）：`serde_json` 解析后把同一句话写进顶层 `status`
    字段再序列化；`action` 保持原值（重放的是作答，不能变成 `cancel`）；解析失败时
    原样返回（绝不产出坏 JSON）。
- i18n（`src-tauri/src/i18n.rs`）新增 `status.replayed`，中英各一句，含「N 秒前」的数值
  拼接（沿用现有 `tr` + `format!` 的写法，不引入新机制）。
- 同批澄清字段说明（spec D7 配套，避免文档把重放误导成取消）：
  - `cli/help.rs` 的 `[status]` 行（中英各一处）改为通用状态说明；
  - `cli/output.rs` 的 `JsonOutput.status` 注释改为通用状态说明，并写明判断取消要看
    `action`（MCP `ask` 已改文本透传、无 `AskResult`，见 spec mcp.md D5 二轮定案）。

## 阶段 5：测试

`cargo test`（单测，随代码就近放）：

- 指纹：message / 问题 / 选项 / `recommended` / `todoId` / 附件顺序 / 各模式位任一不同 →
  指纹不同；仅首尾空白不同 → 指纹相同。
- 会话键：`agent_session_id` 优先、回退 `mcp_instance_id`、两者皆空返回 `None`；
  两类键不互相碰撞。
- `ReplayCache`：窗口内命中 / 超窗不命中 / 指纹不符不命中 / 每会话只留最近一条 /
  第 65 个会话触发 LRU 淘汰。
- `try_attach`：同会话同指纹命中；已 `is_finalizing` 的请求不命中；不同会话不命中。
- 多等待者：`broadcast_outcome` 后每个 follower 都收到等值结果；`release_waiter` 归零
  判定正确。
- 标注：文本模式 `[status]` 区块位置在最前且原有区块逐字不变；JSON 模式 `status` 被填充、
  `action` 不变且仍是合法 JSON；坏 JSON 原样返回。

手工验收（按 spec §5 的 7 条逐条走）：`./scripts/install.sh` 后在 worktree 内用两个终端
模拟重复调用，其中一条用 `kill` 模拟 Cursor 杀进程。

## 阶段 6：文档

- 主 `docs/overview.md`：在「运行架构 / 关键约定」补一句——同会话相同提问合流到一张卡、
  短窗内重放上次答案（跨模块不变量确有变化，必须写）。
- `docs/PROGRESS.md`：实现期间登记进度，完成后删除该 section。
- 用户文档 `docs/wiki/` 不需要改：对用户是纯收敛，没有新的操作面。

## 提交拆分

1. `feat(daemon): coalesce duplicate in-flight asks from the same agent session`（阶段 1–3）
2. `feat(daemon): replay the last answer for identical repeated asks`（阶段 4）
3. `test(daemon): cover ask fingerprinting, coalescing and replay`（阶段 5，若量小可并入前两条）
4. `docs: describe duplicate ask coalescing`（阶段 6）
