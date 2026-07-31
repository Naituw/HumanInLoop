# 开发计划：回复历史 Session 筛选与精确删除

> 关联需求：`docs/specs/reply-history.md` §3.6（F1–F12）
> 状态：已实现（2026-07-31）。
> 本计划是 `docs/plans/reply-history.md` 的增量，不改 ask 写入、stdout、退出码或渠道协调链路。

实现后验证：`pnpm test`（103 项）、`pnpm build`、Rust 全量测试（1050 通过、1 ignored）、
`cargo check` 与 `./scripts/install.sh` 均通过；安装后的 `AskHuman --history --all` 已成功路由到
新 GUI Host。为避免修改用户真实历史，破坏性场景由隔离文件单测覆盖，未对现有记录执行手工删除。

## 0. 目标与现状

历史落盘链路已经记录可选的 `agentSessionId` 与 `mcpInstanceId`，`show_last` 也已按这些字段
读取当前会话；缺口只在历史管理界面：

- 前端 `HistoryEntry` 类型尚未声明两个字段，历史列表、搜索与筛选没有使用它们；
- 后端清理范围只有项目 / 全部，无法精确删除用户已经预览过的任意筛选结果；
- 会话标题解析能力已存在于 `agents/title.rs`，但历史窗口没有批量、缓存化的读取入口。

本机分析时的 200 条现有记录中，190 条带真实 Agent session（23 个不同 session）、8 条只有
MCP instance、2 条无绑定。实现不需要迁移现有 JSONL。

目标交互：

```text
项目 → Session 两级单选菜单 → 关键词 AND 搜索 → 当前显示记录
                                                  ├─ 当前 scope 动作（搜索结果 / 会话 / 项目）
                                                  └─ 清空全部历史
```

弹窗入口另有初始定位优先级：

```text
本次 ask 的真实 Agent session 有既往历史 → 全部项目 + 当前 session
否则本次 ask 的 MCP 兜底分组有既往历史  → 全部项目 + 当前 MCP 分组（身份仍含 project）
否则                                      → 当前项目 + 全部会话
```

## 1. Session 身份与展示模型

### 1.1 前端结构化身份

在 `src/lib/types.ts` 为 `HistoryEntry` 补齐：

- `agentSessionId?: string | null`
- `mcpInstanceId?: string | null`

在 `src/lib/history.ts` 增加纯函数与判别联合类型：

```ts
type HistorySessionRef =
  | { type: "agent"; agentKind: string; sessionId: string }
  | { type: "mcp"; project: string; instanceId: string }
  | { type: "unbound" };
```

归组规则严格遵守 spec F1：

1. 原始记录的 `agentKind` 与 `agentSessionId` 都非空，使用 `agent`；不得用旧记录的 `source`
   推断结果补足真实 session 身份。
2. 否则 `project` 与 `mcpInstanceId` 都非空，使用 `mcp`。
3. 否则使用 `unbound`。

供 `<select>` 使用的 token 由结构化 tuple JSON 序列化产生，例如
`["agent","codex","<id>"]`，不使用手写冒号拼接，避免字段内容造成碰撞。新增纯函数：

- `historySessionOf(entry)`：计算结构化身份；
- `historySessionToken(ref)`：生成稳定、不碰撞的 UI token；
- `groupHistorySessions(entries, titles)`：按身份聚合 `count` / `lastMs` / 展示信息并倒序；
- `matchesHistorySession(entry, token)`：执行 session 单选过滤；
- `shortSessionId(id)`：统一短 ID 展示。

「无会话信息」只是一个筛选分组，不宣称这些记录属于同一真实 session；后续删除仍按可见 entry
ID 精确执行，因此不会把该分组作为后端身份范围。

### 1.2 会话标题批量解析

新增只读 Tauri 命令，例如：

```text
resolve_history_session_titles([{ token, agentKind, sessionId }, ...])
  -> [{ token, title }]
```

- 前端只提交当前项目范围内去重后的真实 Agent session；MCP / unbound 不解析标题。
- 后端只接受已知 `AgentKind`、非空 session ID，并设合理的单次去重上限，随后复用
  `agents::title::resolve_title`。
- GUI Host 进程内增加小型缓存；命中直接返回，失败结果使用短 TTL，避免历史实时刷新时反复扫描，
  同时允许刚创建的会话稍后出现标题。
- 命令是 best-effort：单个标题失败不使整批调用失败。UI 显示
  `Agent + 标题 + 短 ID + 条数`；失败时显示 `Agent + 短 ID + 条数`。
- 前端保留已解析 title map；历史更新时只请求新出现或需要重试的 session，不阻塞首屏列表展示。

标题与完整 session ID 加入现有搜索 haystack。标题是展示增强，不参与 session 身份或删除判定。

### 1.3 弹窗历史定位目标

定义可跨 daemon → Popup Helper → GUI Host 传递的结构化 `HistoryOpenTarget`，只含两种可信目标：

```rust
enum HistoryOpenTarget {
    Agent { agent_kind: String, session_id: String },
    Mcp { project: String, instance_id: String },
}
```

它与前端 `HistorySessionRef` 的 `agent` / `mcp` 一一对应；`unbound` 不作为入口目标。目标必须来自本次 ask
最终会写入 `HistoryEntry` 的原始绑定，不能使用 `agent_console_session_id`：后者要求 session 仍命中活动
`AgentRegistry`，只是“打开 Agent 控制台”能力门控，不能代表历史身份是否可信。

## 2. 历史存储层：按 entry ID 精确删除

在 `src-tauri/src/history.rs` 新增可测试的精确删除 API：

```rust
pub fn delete_ids(ids: &[String]) -> Result<usize, HistoryError>
```

实现要求：

- 空 ID 集合直接返回 0，不触碰文件；
- 将输入转为 `HashSet`，持有现有跨进程历史锁后读取全部行；
- 删除路径使用严格读取：文件不存在视为空历史，其它读取错误立即返回，不能沿用展示读取的
  “失败即空列表”并把原文件覆写为空；
- 只滤除 `entry.id` 在快照集合中的行，使用现有临时文件 + rename 原子写回；
- 返回实际删除数；目标已因裁剪 / 其它操作消失时自然少于请求数；
- 用户主动发起的删除应把锁或写入失败返回上层，不能沿用 ask 记录旁路的静默失败语义；
- 新写入与删除仍由同一把锁串行化。若新记录在 ID 快照形成后写入，它不在集合中，必须保留。

另提供严格的 `clear_all() -> Result<usize, HistoryError>`：在同一把锁内读取当前条数、清空并返回实际
删除数，文件不存在返回 0，其它错误向上传播。现有项目级清理没有其它消费者；迁移历史窗口后删除
`ClearScope::Project` 及相关死代码，不再暴露“清当前项目”。

## 3. Tauri 命令与前端 IPC

在 `src-tauri/src/commands.rs` 与命令注册处新增 / 调整：

- `delete_history_entries(ids: Vec<String>) -> Result<usize, String>`；
- `clear_all_history() -> Result<usize, String>`，替换参数含糊的旧 `clear_history(all, project)`
  命令与前端包装；
- `resolve_history_session_titles(...)` 批量标题命令。

在 `src/lib/ipc.ts` 增加对应的类型安全封装。删除命令只接受 entry IDs，绝不接受项目、session token、
搜索词或“重新执行筛选”参数；这是“删掉用户刚刚看到的那些记录”的核心不变量。

### 3.1 Popup / GUI Host 初始定位透传

补齐入口上下文：

- `ipc::ShowPayload` 新增默认兼容的原始 `agent_session_id` / `mcp_instance_id`；既有 `agent_kind` 继续复用。
  普通 ask、whats-next 与 Agent confirm 构造 `ShowPayload` 时都从其真实调用上下文填入。
- `AppState` 与 `WarmPopup.show` 保留这些字段；冷 Helper、预热 Helper和非 Unix 单进程路径都要接通。
  单进程路径从已有 `CallerContext` / `HistoryBinding` 取值，不能因为没有 daemon 就退回项目。
- `open_history` 新增纯函数式 helper，从 Warm Popup 当前 `ShowPayload` 或冷 Popup `AppState` 生成
  `HistoryOpenTarget`；真实 Agent 绑定优先，缺失时才生成 MCP target。
- GUI Host `HostMsg::OpenWindow` 新增 `history_target: Option<HistoryOpenTarget>`（`serde(default)` 保持
  旧消息兼容），同时继续携带 `project` 作为无命中时的回退项目。不得复用 Interject / Agent Window 的
  `session`、`agent`、`cwd` 槽位拼装历史目标。
- `create_history_window` 接收 target。首次创建时把结构化字段 URL encode 到初始 query；已有全局历史窗口时
  不直接 return，而是 emit `history-open-target` 事件后聚焦。

CLI `AskHuman --history [--all]`、托盘与其它非弹窗入口不携带 target，维持现有项目 / all 默认语义。

## 4. `HistoryView.vue` 状态与交互

### 4.1 筛选流水线

新增状态：

- `selectedSession`：默认特殊 token `ALL_SESSIONS`；
- `sessionTitles`：真实 Agent session token → 标题；
- `pendingDeleteIds`：打开确认框时冻结的可见 ID 快照；
- `confirmKind`：`null | "scope" | "all"`。

计算顺序：

1. `entries` 一次加载全部项目历史，项目切换不再重新读取；
2. `projectEntries` 在客户端应用项目单选；
3. `sessionOptions` 从 `projectEntries` 聚合，**不受关键词影响**，默认项为“全部会话”；
4. `sessionEntries` 应用 session 单选；
5. `filteredEntries` 在 `sessionEntries` 上应用现有多关键词 AND 搜索；
6. 左侧列表、active entry 与当前 scope 条数取相应计算结果。

项目切换或实时更新使当前 session 不再存在时，回退“全部会话”。搜索或刷新导致 active entry 不可见时，
沿用当前逻辑选择第一条可见记录。

### 4.2 项目 → Session 两级范围菜单

顶部只放一个范围按钮，显示当前「项目名 · session 名」。展开后左侧为项目顶层菜单，指向右侧真正的
session 子菜单；每个项目的子菜单包含：

- 首项按项目范围写“本项目全部”或“所有会话”，随后是分隔线；
- 真实 Agent session：Agent + best-effort 标题 + 短 ID；
- MCP 兜底：明确标注“MCP 连接 / 近似会话” + 短 ID；
- 无会话信息。

具体 session 按 `lastMs` 倒序并显示 count。当前项目可通过 hover / 键盘 focus 打开对应子菜单，点击右侧
项目 + session 组合才提交筛选。同一真实 Agent session 在“全部项目”范围内合并展示；MCP 分组始终包含
project 维度。切换范围只改变客户端列表，不发起新的历史读取；标题返回后只更新选项与搜索 haystack，
不改变 token 或当前选择。

### 4.3 清理菜单与确认

顶部按钮从“清空”改为“清理”，菜单最多保留两项：

1. 当前 scope 动作：有关键词时是**删除当前搜索结果（N 条）**；无关键词且选择具体 session 时是
   **清空所选会话的记录（N 条）**；无关键词且选择某项目全部会话时是**清空所选项目的记录（N 条）**。
2. **清空全部历史（N 条）**：不受当前项目、session、关键词影响。

在“全部项目 · 全部会话”且无关键词时，两项语义相同，只显示“清空全部历史”。

点击 scope 动作时立即复制该上下文对应的当前 entry IDs 到 `pendingDeleteIds`，随后打开确认框；确认框显示
动作专属标题、冻结条数以及项目 / session / 关键词摘要。确认执行只发送该数组。确认期间即使收到
`history-updated` 并重算出新的匹配记录，也不能修改 `pendingDeleteIds`。

成功后重新加载项目、历史和 session options，显示实际删除条数；失败则保留当前列表并显示错误，不能假装
已删除。全量清空继续使用独立确认文案。确认取消时清空 `pendingDeleteIds`。

### 4.4 弹窗入口定位与既有窗口 retarget

`HistoryView` 在注册 `history-open-target` 监听后处理首次 URL target；两条路径复用同一个
`applyOpenTarget(target, fallbackProject)`：

1. 先加载全部项目历史，并用与两级范围菜单完全相同的 `historySessionOf` / token 规则查找目标；
2. 若存在至少一条目标记录：保持全部项目数据，项目选择设为 `ALL`，session 设为 target token；真实 Agent
   session 因此展示跨项目的完整记录，MCP token 自身仍含 project，不会跨项目误合并；
3. 若完全无目标记录：按已经确认的产品语义加载 `fallbackProject`，项目选回当前项目，session 设为
   `ALL_SESSIONS`；不保留一个 count=0 的虚拟 session 选项；
4. 无 target 时直接执行第 3 步；
5. 重新定位后选中第一条可见记录，并异步补齐新范围的 session 标题。

全局单窗收到 retarget 时以最后一次入口为准。若筛选删除确认框尚未提交，先关闭确认并清空
`pendingDeleteIds` 再切筛选，避免用户在新 session 视图上确认删除旧快照；已经发给后端的删除请求不取消、
完成后按正常实时刷新合并结果。

## 5. 文案与可访问性

在 `src/i18n/zh.ts` / `en.ts` 增加：

- 全部会话、无会话信息、MCP 近似会话、两级范围菜单 accessible label；
- “清理”、搜索 / 会话 / 项目三个 scope 动作和全量清空；
- 各 scope 与全量的确认标题 / 描述 / 按钮、实际删除数、失败提示；
- 会话标题缺失与短 ID 的辅助文案（如实际 UI 需要）。

要求：

- 搜索动作必须写“删除当前搜索结果”，避免被理解成重置筛选条件；
- MCP 分组不能只显示成“session”；
- 范围按钮、顶层项目与 session 子菜单均有可读 label / title，短 ID 不是唯一可访问名称；
- 删除按钮的 N 与冻结 ID 数一致，不取确认期间实时变化后的列表长度。

## 6. 测试计划

### 6.1 Rust 单元测试

扩充 `history.rs`：

- 按若干 ID 删除，未命中条目保留并返回正确删除数；
- 空数组不写文件；重复 / 不存在 ID 不影响结果；
- 先冻结旧 IDs、再追加一条“同样会命中筛选”的新记录，执行删除后新记录仍在；
- 历史文件读取失败时返回错误且不覆写原文件；
- 删除与全量清空保留 JSONL 坏行容错、锁和原子写既有行为；
- 写入失败能由用户命令感知，ask 记录旁路仍保持 best-effort。

标题批量命令的纯逻辑测试：未知 Agent、空 ID、重复请求、部分解析失败、缓存命中 / 过期；不依赖用户真实
Agent 目录。

入口绑定与 GUI Host 协议测试：

- 有真实 Agent session 时优先于同时存在的 MCP instance，且不依赖 `agentConsoleSessionId`；
- 只有 MCP instance 时构造包含 project 的 target；两者皆空时无 target；
- `ShowPayload` / `HostMsg::OpenWindow` 旧 JSON 缺新字段仍可反序列化；
- 已有历史窗口时走 retarget 事件而非只聚焦。

### 6.2 前端 Vitest

新增 `src/lib/history.test.ts`，至少覆盖：

- 同时有 Agent session 与 MCP instance 时优先 Agent；
- 只有 `agentSessionId`、缺原始 `agentKind` 时不得用 `source` 猜测真实身份，改走 MCP / unbound；
- MCP token 包含 project，同一 instance 在不同 project 不合并；
- token 对包含分隔符的字段仍不碰撞；
- session 分组 count / lastMs / 倒序与 unbound；
- session、关键词 AND 搜索共同生成正确的可见 ID 列表；
- title 成功与短 ID 回退不改变 session token。

如组件测试基础设施允许，补 `HistoryView` 测试：打开删除确认后注入一条新历史，断言传给 IPC 的仍是确认前
ID 快照；验证过滤结果为空时删除项禁用、全量清空仍可用；验证弹窗 target 命中时切到“全部项目 + 当前
session”、零命中时回退“当前项目 + 全部会话”，以及 retarget 会取消未提交的旧删除确认。

### 6.3 构建与端到端

实现完成后按项目规则验证：

1. `pnpm test`
2. `pnpm build`
3. `cargo test --manifest-path src-tauri/Cargo.toml`
4. `cargo build --manifest-path src-tauri/Cargo.toml`
5. `./scripts/install.sh`
6. 使用新安装的 `AskHuman` 打开历史窗口验证；删除场景只使用专门创建的可丢弃测试记录或隔离的
   Dev Instance，未经确认不改动用户既有真实历史：
   - 当前项目 / 全部项目下 session 分组、标题、MCP / unbound 标签；
   - 从普通 ask / whats-next / Agent confirm 弹窗进入时优先当前 session；跨项目 session 展示完整，
     无历史与无绑定时回退当前项目；
   - 历史窗口已打开后从另一 session 的弹窗再次进入，既有窗口正确 retarget；
   - 项目 → session 两级范围菜单、多关键词组合筛选与结果条数；
   - 搜索 / 会话 / 项目 scope 清理后，未显示记录与确认期间新写入记录保留；
   - 清空全部历史仍独立可用；
   - 删除后相应 `show_last` 不再恢复，而 daemon 5 分钟重放和在途卡片不受影响；
   - 历史实时刷新、主题、中英文与空态无回归。

## 7. 实施顺序与提交边界

### Phase 1：纯模型与精确删除核心

1. TS `HistoryEntry` 补字段；`lib/history.ts` 增 session ref / token / grouping / match 纯函数与 Vitest。
2. Rust `history::delete_ids`、错误传播、实际删除数与竞态单测。

验收：无需 UI 即可证明身份优先级与“只删 ID 快照”的核心不变量。

### Phase 2：标题与 IPC

1. 批量标题请求 / 响应类型、解析校验与进程缓存。
2. Tauri 命令注册和 `lib/ipc.ts` 包装。
3. 精确删除与全量清空命令接线。
4. `ShowPayload` / `AppState` / GUI Host `HistoryOpenTarget` 透传及旧协议兼容测试。

验收：命令层能批量返回部分标题、按 ID 删除并报告实际数量 / 错误。

### Phase 3：历史窗口交互

1. 项目 → session → 关键词计算链路与两级范围菜单。
2. 标题异步补齐、搜索 haystack 扩展。
3. 首次 URL target 与既有窗口 `history-open-target` retarget，含命中 / 零命中回退。
4. 搜索 / 会话 / 项目当前 scope 动作的动态文案、ID 冻结 / 确认 / 错误态。
5. 保留“清空全部历史”，全量无搜索时隐藏重复的当前 scope 动作。
6. 中英文文案、样式与组件测试。

验收：用户删除前能在左侧列表完整看到目标集合，实际删除集合与预览一致。

### Phase 4：文档、安装与真实数据验收

1. 把 `docs/specs/reply-history.md` 的扩展状态改为已实现。
2. 更新 `docs/overview.md` 历史命令 / 筛选能力，以及 `docs/wiki/settings.md(.en)` 用户说明；旧总计划
   `docs/plans/reply-history.md` 如有冲突处加当前实现注记，不重写历史过程。
3. 删除 `docs/PROGRESS.md` 中本事项。
4. 完成 §6.3 的安装后端到端验证。

## 8. 风险与明确不做

- **标题扫描成本**：必须批量去重、缓存并允许部分失败；标题不能阻塞历史首屏。
- **删除竞态**：禁止后端按筛选条件重算；唯一可信删除目标是确认时冻结的 entry ID。
- **单窗陈旧筛选**：历史窗口已存在时必须显式 retarget；仅 focus 会让用户误以为看到的是当前弹窗 session。
- **身份可信度**：MCP instance 只是进程级兜底，不保证等价于一段原生 Agent 对话。
- **共享恢复源**：删除 JSONL 会影响 `show_last`，这是既定语义；UI 确认文案无需承诺可恢复。
- **不动 daemon 会话状态**：不清重放缓存、不取消待答卡、不结束 Agent session、不修改 lifecycle registry。
- **不迁移旧记录**：不从 transcript、项目或来源名反推历史行的 session ID。
- **不新增多选 session**：首期保持单选，组合范围由项目与关键词完成。
- **不改容量裁剪**：`general.historyLimit` 和按最近 N 条裁剪逻辑保持不变。
