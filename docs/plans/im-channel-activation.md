# IM 渠道按「工作会话」自动激活 — 设计与实现计划

> 状态：**P1 + P2 已实现**（四渠道接入；飞书真机验证，其余仅编译验证）。
>
> 一句话：电脑前默认只弹窗、IM 零刷屏；离开电脑时，在某个 IM 发一条消息即把后续提问与「在途未答」引到该渠道，并跨重启记住这个选择。

---

## 1. 背景与目标

AskHuman 把每次提问推给多个「人机交互出口」：本地**弹窗（popup）** + 多种 **IM**（钉钉 / 飞书 / Telegram / Slack），由抢答协调器「首个终态结果生效」。

**现状痛点**：每道题**并行推给所有已启用 IM** → 人在电脑前用弹窗答题时，IM 仍被刷屏。

**目标**：
- 在电脑前：默认只弹窗，IM **零刷屏**。
- 离开电脑：提问能可靠送达**某一个** IM 并在该 IM 回复。
- 同一时刻最多激活**一个** IM；切换自然（在哪个渠道说话就用哪个）。
- 跨 daemon 重启保留「当前用哪个渠道」，避免重启后又开始刷屏 / 漏消息。
- 不引入「多台电脑必须各配不同 bot」这类重负担。

---

## 2. 复用现有「Agent 生命周期追踪」机制（不重复造）

「会话是否在工作 / 空闲 / 已结束」的判断**完全复用**已实现的 Agent 生命周期追踪（见
`docs/specs/agent-lifecycle-tracking.md`、`src-tauri/src/agents/`）：

- `AgentRegistry`（`agents/registry.rs`）已维护三态 **工作中 / 空闲 / 已结束**：
  - turn-start → 工作中、turn-end → 空闲（来自三家用户级 lifecycle hook，经 `__agent-hook` 上报）；
  - **进程存活轮询**为权威「已结束」判据；**无 pid 时 1h TTL** 兜底。
- 提问路径已带 `agent_kind / agent_session_id / agent_pid`（PPID 向上 walk 锁定的真实 Agent 进程）。

本特性**只在其上加一层 IM 路由**，不改追踪本身。下面凡说「工作中 / 空闲 / 结束」均指注册表的判定。

---

## 3. 最终模型（锁定行为）

### 3.1 总开关（默认关 = 旧行为）
- 新增独立开关「IM 会话期自动激活」，**默认关**。
- 关：完全保持现状——每次提问**全发**所有已启用 IM（零行为变化）。
- 开：启用本节模型。
- 开关**入口**放在「实验」区（仅 `experimental.enabled` 且非 Windows 显示），但**配置字段不放在 `experimental` 里**（见 §5），便于将来「转正」时已开启用户无需重开。

### 3.2 入站消费（连 IM、收命令）= 有「工作中」会话；**与总开关无关**
- 守护进程**在世期间**就应能收 IM 入站命令（`/here`、`/status`…）。守护进程的存活本就由「工作中
  agent」生命周期约束（D18：无工作 agent + 无连接 + 无订阅才空闲退出），故：
  - 存在「工作中」会话 → 为各已启用 IM **建/保持长连接 + 消费入站**（默认只收、不发卡片）。
  - **不做「反激活式」主动断连**：连接随守护进程退出而释放（serve 收尾丢弃 Router → Drop 关长连接）。
- **与总开关（§3.1）无关**：入站消费支撑的 `/status` 是独立功能；总开关只决定 §3.4 的「切槽 / 发卡」。
- **无 turn hook 的兜底**：总开关**开**时，每次提问把该 session 兜底登记为「工作中」（pid 用 `agent_pid`），
  使没装 hook 也能在提问在途期间驱动入站消费 / 切槽；装了 hook 则 turn-start 即上线。
  （总开关**关**时不兜底登记，尊重「未装 hook = 不追踪」，不污染注册表；此时 `/status` 仅在装了 hook、
  有工作中 agent 时可用——本就是 `/status` 有意义的前提。）

### 3.3 活跃槽（单个 · 持久化 · 统一含「弹窗」· 在哪儿说话就用哪儿）
- 全局**单**活跃槽 `active_channel`，取值为某 IM id 或 `"popup"`（Popup 可用时，`"popup"` / None = 不向任何 IM 发卡片，只弹窗）。
- **持久化、跨 daemon 重启保留**（重启后即便我不在电脑前，仍按旧槽继续收消息）。
- **改变途径 = 我在某渠道「说话」**——统一为「更新活跃槽」一处逻辑（`set_active_channel`）：
  - 在某 IM 发**任意**入站消息 → `active_channel = 该 IM`；
  - 在**弹窗作答 / 取消** → `active_channel = "popup"`（= 人在电脑前，后续不再发 IM）。
  - **不**由 turn-start / 会话结束改变。
- **切槽时统一处理三件事**（都在 `set_active_channel`，非弹窗特例）：
  - 给**旧**渠道（若为 IM）发**反激活**提示，并**点明切到了哪个渠道**（如「已切换到『弹窗』/『钉钉』」），发 /here 可切回；
  - 把**所有在途未答**问题**补推**给**新**渠道（若为 IM）——补推是「渠道激活」的固有行为，**与触发方式无关**（`/here`、普通消息、`/status` 切槽、作答切槽均同）；
  - 新渠道的**激活**提示由调用方按场景发送（IM 入站回执见 §3.4；弹窗无收件端，免发）。
- 发卡片的判据：armed（有工作中会话）**且** `active_channel` 命中某 IM → 新提问同时发该 IM（+ 已启用的 Popup）；否则只弹窗。**可达性兜底**：若 Popup 禁用/无显示，且「有效活跃槽 ∪ watch 渠道」交集为空，该次全发所有可用 IM，避免零投递；不自动改持久化活跃槽，首个回复渠道会按既有逻辑成为新活跃槽。

### 3.4 入站分派：通用 slash 命令机制
入站文本 `trim` 后**以 `/` 开头**才算命令（机制可扩展，后续可注册更多命令）；否则按「普通消息」。命令大小写不敏感。本期内置：

| 入站 | 总开关关 | 总开关开 · 改活跃槽 | 总开关开 · 行为 | 回执 |
|---|---|---|---|---|
| `/here`、`/这里` | **静默忽略**（无活跃槽概念） | 是 | 切槽即随激活补推在途（§3.3） | **总是**回执 |
| `/status`、`/状态` | **照常回状态文本**（独立功能） | 是 | 回**状态文本**（§3.6）；切槽时随激活补推在途 | 仅当槽切换时回执 |
| 其它普通消息（不带斜线） | **不处理**（交卡片作答） | 是 | 切槽即随激活补推在途；**文本不当答案** | 仅当槽切换时回执 |

> 定调：`/status` **与总开关无关**，始终响应；`/here`、切槽、补推仅总开关开时生效。
> 开关开时**任意入站都改槽**；**补推随切槽自动发生**（在 `set_active_channel` 内，不绑定具体命令）；
> 是否回执只看「是不是 here」（here 必回执，其余切换才回执）。
> 此外，凡切槽（含弹窗作答切到 "popup"）都会给**旧** IM 发反激活提示（§3.3）。

### 3.5 回执文案（进 i18n，`autoChannel.*`；不含歧义的「本机」表述）
- 激活（新渠道，here / 切换）："后续提问将发送到此渠道。"（补推 N>0 时追加「（已补推 N 条待答问题）」）
- 反激活（旧渠道，`{target}` = 新渠道展示名）："后续提问已切换到「{target}」，将不在此发送。发送 /here 可切回此渠道。"

### 3.6 `/status` 文本与空状态
- 取 `AgentRegistry` 快照，**仅列 工作中 / 空闲**，已结束不列；「工作中」在前。
- 每行：`类型 — 标题（项目）`，标题/项目缺失各有占位。
- 非空示例：

```
工作中
• Cursor — 重构登录模块（HumanInLoop）
• Claude Code — 修复回归用例（api-server）

空闲
• Codex — （未命名）（web）
```

- 空状态（无工作中/空闲）示例：

```
当前没有工作中或空闲的 agent。
（agent 状态依赖「生命周期追踪」；如未开启，请在 设置 → 高级 中开启对应 Agent 的追踪。）
```

---

## 4. 关键约束与取舍（务必记住）

1. **平台「同一 bot 同一时刻仅一条长连接」**（钉钉 Stream / 飞书 WS / Slack Socket Mode / Telegram getUpdates 单 offset）。本方案靠「IM 仅在本机有工作会话时才连」→ 用户一般不会多机同时跑 Agent → 同一 bot 不会并发连接，抢占问题自然消失。
2. **唯一残留代价**：无中心服务仲裁，本机的 IM 长连接在「有工作会话」期间建立，并保持到**守护进程空闲退出**（无工作 agent + 无连接 + 无订阅后约 5 分钟，D18）才释放——即空闲窗口内仍占着该 bot。多机冲突仅在「两台机器同时有**活着的工作会话**（或处于该空闲窗口）+ 同一 bot」时发生，罕见、可接受。
3. **信号原则**（沿用生命周期追踪）：进程存活＝电平骨干（判会话在不在）；turn-start↔turn-end 成对事件＝判忙/闲；TTL 仅兜底。不用「定时超时升级到 IM」（时长无法定，已否决）。
4. **弹窗按配置参与**：`channels.popup.enabled` 且有显示环境时，它是本地、免费、无刷屏顾虑的默认出口；它不可用且按需发送选不出有效 IM 时，为保证可达性才例外全发所有可用 IM。

---

## 5. 实现分期与落点

### P1（不依赖新增 hook，先可用）—— 已落地，落点如下
1. **配置**：`ChannelsConfig.auto_activation: bool`（camelCase `autoActivation`，默认 false，**不在 `experimental` 里**）。设置页「渠道」Tab 顶部开关 UI + 简短说明（仅 `experimental.enabled` 且非 Windows 显示），i18n。`config.rs`、`SettingsView.vue`、`src/lib/types.ts`、`src/i18n/*`。
2. **活跃槽持久化**：`~/.askhuman/state/auto-channel.json`（`{ "channel": "feishu"|"popup"|null, "updatedAt": <secs> }`，含 `"popup"`）。`paths.rs` 路径 helper；`ServerState.active_channel: Mutex<Option<String>>`，启动 `autochannel::load_active()`、`set_active_channel` 变更原子写。逻辑集中在 `autochannel.rs`。
3. **会话兜底登记**：`AgentRegistry::upsert_working`（建/更新为工作中，补 pid/cwd）；`handle_submit` **开关开时**对带 `agent_kind/session_id` 的提问调用，使无 hook 也能驱动入站消费/切槽（开关关时只 `touch_activity`，不污染注册表）。
4. **发卡门控（attach）**：`daemon::attach_im_channels` 开关开时仅对「有效 `active_channel` ∪ watch 渠道」执行 `register + start`；Popup 不可用且候选为空时，全发所有可用 IM 兜底；开关关 → 旧「全发」逻辑。注意：**「连 IM 收命令」不在 attach**，而由 `ensure_inbound_listeners`（按「有工作中 agent」自门控、与开关无关）负责。
5. **daemon 级入站消费**：各 Router 暴露原始消息观察者——飞书/Slack `observe_message`、钉钉 `observe_bot`、Telegram **新增** `observe_message`（`telegram/router.rs`，并对 armed 时的斜线文字不路由到卡片）。`ensure_inbound_listeners` 用通用 `spawn_listener` 循环 + 各家 `extract_*` 抽取 `(发送者,文本)`，交 `handle_inbound` 按 §3.4 分派。
6. **补推在途**：`backfill_inflight` 枚举 `RequestRegistry::in_flight_entries`，对未挂该渠道的请求 `coordinator.register + Channel::start`（`coordinator.has_channel` 去重）。**由 `set_active_channel` 在切槽时统一调用**（不绑定具体命令）——任何方式激活某 IM 都补推，`set_active_channel` 返回 `(是否切换, 补推数)` 供调用方拼激活回执。
7. **`/status`**：`autochannel::status_text` 由 `agents.snapshot()` 过滤 working/idle 组装（§3.6）→ `reply_channel_text` 经该渠道回消息；i18n。**与开关无关**（§3.2/§3.4）。

### P2（已实现：入站消费随「工作中」起、随守护进程退出而止）
- 复用既有三家 lifecycle hook 的 turn-start/turn-end（`__agent-hook` 通路已上报 turn 事件，见
  `integrations/agent_lifecycle.rs`：Claude `UserPromptSubmit`/`Stop`、Codex 同名、Cursor `beforeSubmitPrompt`/`stop`）。
- daemon 在 `AgentEvent`（turn-start）与每次提问（`handle_submit`）处调用 `ensure_inbound_listeners`
  （自身按 `working_count > 0` 自门控、幂等、**与总开关无关**）：agent 一进入工作即上线 IM 入站消费，
  使 `/here`、`/status` 在工作期间随时可用，无需等它先提问。
- **不做主动断连**：连接随守护进程退出而释放（serve 收尾 `*fs_router=None…` → Drop 关长连接）；
  守护进程的存活已由「工作中 agent」生命周期约束（D18）。即「同 bot 单连接」在工作结束、守护进程
  空闲退出后自然让出（容忍空闲窗口内仍占用，符合用户拍板）。
- 未装 hook 时由 `handle_submit` 的 `upsert_working` 兜底（仅总开关开时；提问在途即驱动消费 / 切槽）。
- **Codex 注意**：需用户在 Codex 侧启用实验 hooks，否则 turn 事件不上报、退回提问兜底。

### 已实现的边界处理
- **Telegram 自由文字双重身份**：Telegram 的自由文字既是卡片答案、又会被 daemon 观察者收到。armed 时
  `telegram/router.rs` 对**斜线前缀**文字仅交观察者、不路由到在途卡片（避免 `/here` 被当成答案）；
  非斜线文字仍正常作答（飞书/Slack 卡片输入在卡内，无此问题）。
- **卡片渠道的普通文字非答案**：飞书/钉钉的作答来自**卡片输入框 + 提交按钮**；聊天里的普通文字仅用于
  「附带图片/文件」，**不作为答案**（见 `channels/feishu.rs` 卡片路径对 `FsInbound::Message` 的处理）。
  故关态下在飞书/钉钉聊天打字不会答题；只有 Telegram（纯文本、无交互卡片）的普通文字才是答案。

### 仍待办（暂不做 / 阻塞）
- 钉钉 / Slack / Telegram 真机验证（编译通过；当前仅飞书有配置）。
- 多机同 bot 检测提示；turn-end 且人不在 → 主动「该你回复了」通知（待定，暂不做）。

---

## 6. 相关文件清单（速查）

- `docs/specs/agent-lifecycle-tracking.md` —— 复用的生命周期追踪机制。
- `src-tauri/src/autochannel.rs` —— 与传输无关的核心：活跃槽持久化、slash 命令解析、`/status` 文本、激活/反激活回执。
- `src-tauri/src/agents/{registry,report,detect,mod}.rs` —— 注册表（含 `upsert_working`）、上报、PID walk。
- `src-tauri/src/daemon/mod.rs` —— `handle_submit` / `attach_im_channels` / `ensure_inbound_listeners` + `spawn_listener` + `extract_*` / `handle_inbound` / `set_active_channel` / `backfill_inflight` / `build_im_channel` / `reply_channel_text` / `ServerState`（`active_channel`、`inbound_listeners`）。
- `src-tauri/src/app/coordinator.rs` —— 抢答协调；`has_channel`（补推去重）、`winner_channel_id`（作答后切槽）。
- `src-tauri/src/daemon/request.rs` —— `in_flight_entries`（补推枚举在途）。
- `src-tauri/src/channels/{mod,conversation,dingding,feishu,telegram,slack,popup}.rs`、`{dingtalk,feishu,slack,telegram}/router.rs` —— 长连接与入站观察者（`observe_message`/`observe_bot`）。
- `src-tauri/src/config.rs`、`src/views/SettingsView.vue`、`src/lib/types.ts`、`src/i18n/*`、`src-tauri/src/i18n.rs` —— 开关与文案。
- `src-tauri/src/paths.rs` —— `state_dir()` + `auto_channel_file()`（`state/auto-channel.json`）。
