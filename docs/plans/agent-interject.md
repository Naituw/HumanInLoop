# 实现计划：Agent 插话（Interject）

> Windows 注（2026-08）：本文保留首期 Unix 实施步骤；Windows 已由平台对齐项目复用同一 daemon
> 队列、Hook 与 GUI Host，以下 Unix-only 描述仅作历史记录。

> 需求/调研/定案见 `docs/specs/agent-interject.md`（D1–D9）。本计划按里程碑拆解，
> 每个里程碑可独立编译、单测通过。Unix only；Grok 全程排除（D1）。

## M1 daemon 核心：插话队列 + IPC + hook 协议

**目标**：hook 三态协议端到端可用（无 UI，可用临时 CLI/单测驱动）。

1. **`src-tauri/src/agents/interject.rs`（新）**：`InterjectStore`
   - `HashMap<session_id, Entry { entries: Vec<InterjectEntry { text, attachments }>, composer_open, waiters }>`；
   - 操作：`submit(session, text, attachments)`（弹窗提交，整体覆盖）/
     `append(session, text, attachments)`（控制台/IM）/ `clear(session)`（撤回）/
     `poll(session) -> PollOutcome`（原子出队：非空文本按空行拼接、附件路径展平，
     并发 waiter 只有一个拿到）/ `composer_open/close(session)` / `full_text(session)`（预填）；
   - 提交时唤醒该 session 全部 waiter：一个 `Message(delivery)`、其余 `Release`；取消/关闭唤醒全部
     `Release`；socket 写失败用原始 delivery 回队，保留条目边界与附件；
   - 持久化 `~/.askhuman/state/interject.json`（`paths.rs` 加 `interject_file()`）：只存 entries；旧版
     `sessions: HashMap<session_id, Vec<String>>` 保留，新增按条目对齐的可选附件 map，
     **仅在变更时**原子写（复用 watch.rs 的写法），daemon 启动 `load()` 一次；会话 ended 时清理（D8）。
2. **IPC（`src-tauri/src/ipc/mod.rs`）**：
   - `ClientMsg::AgentEvent` 增 `#[serde(default)] interject_poll: bool`；
   - `ClientMsg` 新增：`InterjectComposer { session_id }`（宿主 composer 窗口连接发送；连接断开视为
     close）/ `InterjectSubmit { session_id, text, attachments }` /
     `InterjectAppend { session_id, text, attachments }` / `InterjectClear { session_id }`；
   - `ServerMsg` 新增：`InterjectDecision { action: "none"|"message"|"hold"|"release", text, attachments }`
     （首帧 none/message/hold；hold 后二帧 message/release）；
   - `AgentsState` 快照每条记录注入 `pendingInterject: bool`（AgentsView 徽标用）。
3. **daemon（`daemon/mod.rs`）**：
   - `handle_agent_event` 对 `interject_poll=true` 且事件为 activity(PreToolUse) 的连接按 D3 三态回帧；
     `Hold` 时把 waiter 挂进 store、连接断开自动移除；该连接与 composer 连接均**抵消空闲保活计数**
     （类比 `handle_tray_sub`）；
   - 会话 `ended`（registry 状态迁移处）→ `store.clear(session)`；
   - store 变更 → 触发 `broadcast_to_guis(AgentsState)` 与 `broadcast_tray_state`（徽标/菜单刷新）。
4. **单测**：store 的覆盖/追加/出队原子性/并发 waiter 唯一交付/持久化 round-trip；IPC serde 兼容
   （旧消息缺 `interject_poll` → false）。

## M2 hook 侧：reporter 扩展 + 安装产物长超时

**目标**：三家（Claude/Codex/Cursor）PreToolUse hook 完成三态协议与等待。

1. **`agents/report.rs`**：intended∈{claude,codex,cursor} 且通过既有去重、事件为 activity 且
   stdin 判定为 **pre**（复用 `extract_tool` 的 pre/post 判定）时，`AgentEvent.interject_poll=true`；
   发送后读首帧（**300ms 超时**，超时/断连/旧 daemon 无回包 → 直接退出＝allow，D4）：
   - `none` → 退出（无输出）；
   - `message` → 按家族输出 deny JSON（格式与 `[USER INTERJECTION]` 文本及附件路径块见 spec D3，
     `prompts.rs` 单一来源；Cursor 另带本地化 `user_message`）后 exit 0；
   - `hold` → 无限期读二帧（受 hook 自身 timeout=86400 兜底）：`message` → deny JSON；`release` → 退出。
   - 注意：现有 `report_agent_event` 即发即走，需为 poll 场景改为「发送 + 读回」的变体，仍在
     current-thread runtime 内完成。
2. **`client/mod.rs`**：新增 `report_agent_event_with_poll(...) -> PollOutcome`（内部复用
   `ensure_running` + 单连接；读帧带超时参数）。
3. **`integrations/agent_lifecycle.rs`（D5，含已开启用户的更新流程）**：
   - 三家 PreToolUse 条目加 `"timeout": 86400`（Claude settings.json / Cursor hooks.json /
     Codex hooks.json；仅 PreToolUse，其余事件不动；Grok 不动）；
   - `codex_trusted_hash` 按条目实际 timeout 计算（PreToolUse=86400、其余 600），
     `codex_trust_entries` 从 hooks.json 读 timeout 字段（缺省按 600），`[hooks.state]` 随安装重写；
   - **过期判定扩展**：`json_status` 的 `complete` 口径加「PreToolUse 条目 timeout==86400」
     （`elem_cmd_equals` 处顺带校验）；Codex 信任哈希口径随 timeout 变化自动判 outdated →
     已开启用户由 daemon 启动的 `migrate_outdated()` 自动幂等重装（零手动），设置页「需更新」按钮、
     CLI `agents update --lifecycle`、`doctor` 为手动兜底（同一 `status()` 口径，无需另改）；
   - 单测：产物含 timeout 字段、hash 参考值重算（timeout=86400）、**旧产物（无 timeout）判 outdated**、
     migrate 后判定归位。
4. **验证**：单测 + `install.sh`；hook 侧行为用「假 daemon」单测（bind 临时 socket 回各帧）覆盖
   三态与 300ms fail-open。**不实测真实 agent**（计费红线；留待用户验收）。

## M3 GUI：composer 窗口 + AgentsView 入口

1. **宿主路由（`gui_host/mod.rs`）**：`WindowKind::Interject`；`HostMsg::OpenWindow` 增
   `session: Option<String>`（兼容 `project` 的既有模式）；`host_open` 透传。
2. **窗口（`app/mod.rs` + `app/gui_host.rs`）**：`create_interject_window(session_id, agent_label, ...)`
   ——**每 session 全局唯一**（窗口 label 带 session 短哈希，存在则聚焦），弹窗风格尺寸；
   兜底路径（非宿主进程）同现有 settings/history 模式。
3. **前端**：
   - `App.vue` 路由 `?view=interject&session=...`；
   - `views/InterjectView.vue`（新）：头部（agent 类型胶囊 + 项目名 + 状态）+ 多行输入框（预填
     `full_text`）+「发送 / 取消」+ 待送达提示；挂载即经 IPC 向 daemon 登记 composer_open、
     卸载/取消登记 close；⌘↵ 提交、Esc 取消；
   - `commands.rs` + `lib/ipc.ts`：`interject_init(session)`（预填文本 + agent 摘要 + 主题/语言）、
     `interject_submit(session, text, file_paths, pasted_images)`、`interject_cancel(session)`、`interject_clear(session)`、
     `open_interject(session)`（AgentsView 按钮 → host_open 路由）。命令内部经 daemon 连接实现，
     该连接生命周期与窗口一致（断开＝composer 关闭，D7）。
   - composer 与 Agent 控制台支持选择/拖入源文件、粘贴图片和仅附件发送；源文件只记录绝对路径，
     剪贴板图片写到问答弹窗共用的 24 小时请求临时目录，不建立长期托管目录。IM 入口保持纯文本。
4. **AgentsView**：非 grok、非 ended 卡片加「发送消息」按钮（`open_interject`）；
   `pendingInterject` 徽标（「待送达」）+ 撤回按钮（行内二次确认，`interject_clear`）。
5. **i18n**：zh/en 全量新键（按钮、窗口标题、占位、徽标、撤回确认、托盘项）。

## M4 托盘：Agent 子菜单

1. **`ipc/mod.rs`**：`TrayState` 增 `#[serde(default)] agents: Vec<TrayAgentInfo>`
   （`{ session_id, kind, project_name, state, pending_interject, focusable(终端可聚焦), pid }`，
   工作中在前、ended 不含；旧宿主缺字段 → 空）。
2. **daemon**：`broadcast_tray_state` 组装 agents 摘要（registry snapshot + terminal 支持度 +
   interject store）。
3. **`app/gui_host.rs` + `app/tray_menu.rs`**：「Agent 状态」父项改子菜单（D7）——
   首项 `open_agents`（打开状态窗口）+ 分隔线 + 每 agent 一个子菜单（key 用 session_id 保 diff 稳定）：
   「发送消息」（非 grok；点击 → 宿主本进程 `create_interject_window` 或 host 内直开）、
   「聚焦终端」（`focusable` 时显示；宿主进程直接调 `integrations::terminal_focus`）。
   `menu_signature` 纳入 agents 摘要；结构变化走既有最小 diff。
4. **单测**：tray_menu diff 用例补「agent 子菜单增删/徽标文字变化」。

## M5 IM `/msg`（可与 M3/M4 并行，最后接线）

1. **`autochannel.rs`**：命令解析增 `Msg(seq, Option<String>)` / `MsgClear(seq)`
   （中文别名 `/插话`、`/撤回`；Slack 前缀规则沿用 `!`）；
   - `/msg <n> <内容>` → `store.append`，回执「已排队，共 N 条待送达」；
   - `/msg <n>` → 回显全文（空则提示无待送达）；
   - `/msg-clear <n>` → 清空 + 回执；
   - grok 会话 → 回「该 agent 不支持插话」；编号寻址复用 `/status` 的 seq；
   - help 文案（开关无关，与 `/status` 同门控）。
2. **i18n** + 单测（命令解析、门控、grok 拒绝）。

## M6 收尾

1. `docs/overview.md` 增插话小节（架构/触点/性能要点）；`docs/PROGRESS.md` 标记待验收；
2. 全量 `cargo test` + `vue-tsc` + `install.sh`；
3. 留给用户的验收清单（需真实 agent，AI 不实测）：
   - 三家各跑一次：排队消息在下一次工具调用被 deny 送达、模型复述内容；
   - composer 打开时 hook 等待（观察工具调用暂停）、提交/取消两分支；
   - Cursor 超时放行后消息不丢（留队）；
   - 弹窗二次打开预填可覆盖；AgentsView 徽标/撤回；托盘子菜单两动作；
   - daemon 重启后待送达恢复；会话结束清空；
   - Cursor 双触发去重下不重复送达（`~/.claude` 兼容 hook 不 poll）。

## 技术要点备忘（跨里程碑）

- **性能红线（spec §4）**：hook 热路径零文件 IO；无插话时增量＝一次 UDS 往返 + daemon O(1) 查表；
  首帧 300ms fail-open；PostToolUse 不 poll；未开生命周期＝零成本。
- **去重**：poll 只在 `running == intended` 的那次上报执行（Cursor 兼容加载 `~/.claude` 的
  双触发由既有 `report.rs` 判据拦截），避免同一工具调用两个 hook 同时 poll/deny。
- **保活**：poll 等待连接与 composer 连接都不得给 daemon 续命（抵消 `active` 计数，类比 TraySubscribe）。
- **并发**：`take` 原子出队保证多 waiter/多 poll 只有一处交付；其余 allow/release。
- **兼容**：所有 IPC 增量走 `serde(default)`/忽略未知字段，旧 CLI/旧 daemon 双向不炸；
  hook 对旧 daemon（不回帧）300ms 放行。
- **deny 包装文案**：`prompts.rs` 单一来源（英文），Claude/Codex 用 `permissionDecisionReason`、
  Cursor 用 `agent_message`（`user_message` 给简短本地化提示）。
