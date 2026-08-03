# 实现计划：Agent 生命周期追踪 + 状态窗口（实验性功能）

> 关联需求：`docs/specs/agent-lifecycle-tracking.md`（决策编号 D1–D24）
> 关联调研：`demo/agent-lifecycle/FINDINGS.md`（事件 / env / 去重 / 标题来源 / Codex 信任算法，均实测）
> 平台：仅 Unix（macOS/Linux）；Windows 全程编译进去但 UI 与命令对用户隐藏（D2）。
> 分支：建议 `feat/agent-lifecycle-tracking`（基于当前 daemon 架构分支线）。

整体数据流：

```
agent 进程
  └─(hook 触发)→ AskHuman __agent-hook <agent> <event>   # 短命子进程，读 stdin、去重、walk pid
                       └─(IPC)→ daemon: AgentRegistry.apply_event()
                                   ├─ 存活轮询 kill-0 / TTL 兜底 → 推导 工作中/空闲/已结束
                                   ├─ 持久化 ~/.askhuman/agents.json
                                   └─(推送 AgentsState)→ agents status 窗口（订阅者，动态刷新）
agent 调 AskHuman 提问 → client 顺带上报身份 → 刷新最近活动 + 重置 TTL（D21）
```

实现顺序按「**先打通核心数据流（无 UI、无安装）→ 再做窗口 → 再做安装与设置 UI → 最后实测/文档**」。每阶段可独立 `cargo test` / `npm run build` 验证。

---

## Phase 0：进程识别 + 标题解析（共享工具，纯函数优先）

新增 `src-tauri/src/integrations/agent_detect.rs`（或 `src-tauri/src/agents/` 模块，二选一在编码时按模块归属定；下文统称 detect 模块）：

- `detect_running_agent(env) -> Option<AgentKind>`：按 D22 顺序判定（`CURSOR_*`→Cursor、`CODEX_*`→Codex、`CLAUDECODE`→Claude；`CLAUDE_PROJECT_DIR` 不可作判据）。判不出返回 `None`。
- `session_id_from_env(kind, env) -> Option<String>`：Claude=`CLAUDE_CODE_SESSION_ID`、Cursor=`CURSOR_CONVERSATION_ID`、Codex=`CODEX_THREAD_ID`。
- `walk_agent_pid(start_pid, kind) -> Option<u32>`：向上 walk 进程树找 agent 进程 pid。复刻 `demo/agent-lifecycle/harness/common.cjs` 的逻辑：`ps -o ppid=,comm=,command=` 逐级回溯；按 comm / argv0 basename 匹配（Cursor=`agent`/`cursor-agent`、Codex=`codex`、Claude=`claude`），排除自身 marker。Linux 可优先读 `/proc/<pid>/stat`。失败返回 `None`（落 TTL 兜底 D12）。
- `kill0(pid) -> bool`：`libc::kill(pid, 0)`（crate 已依赖 `libc`）。

新增 `AgentKind { Claude, Codex, Cursor }`（serde rename：claude/codex/cursor），含 `as_str`/`parse`。

新增标题解析（同模块或 `agents/title.rs`），按 D10：

- `resolve_title(kind, session_id) -> Option<String>`：
  - Cursor：glob `~/.cursor/chats/*/<sid>/meta.json` 读 `.title`；缺失则读 `~/.cursor/projects/*/agent-transcripts/<sid>/<sid>.jsonl` 第一条 `role==user` 的 `message.content[].text`。
  - Codex：glob `~/.codex/sessions/**/rollout-*-<sid>.jsonl`，取第一条 `payload.role==user`（或 `type==response_item` 且 role=user）的文本，**跳过** `<environment_context>` / `<user_instructions>` 等以 `<` 开头的注入块。
  - Claude：glob `~/.claude/projects/*/<sid>.jsonl`，取最后一条 `type==summary` 的 `summary`；否则第一条 `type==user` 且非 `isMeta` 的文本（同样跳过 `<...>` 注入块）。
  - 统一截断到合理长度（如 80 字），去换行；全空返回 `None`（窗口显示「(未命名)」）。
- 读文件 best-effort、容错（文件可能正被写 / 巨大 → 只读前若干行 / 末若干行）；标题解析放 daemon 侧、按 `session_id` 缓存，快照构建时惰性补齐（标题可能由 agent 异步生成、后续快照再出现）。

测试：detect 与 title 解析尽量做成纯函数（输入 env map / 文件内容字符串），加单测覆盖三家样例（可从 `demo/agent-lifecycle` 取真实片段）。

---

## Phase 1：IPC 协议 + daemon Agent 注册表（核心，无 UI、无安装）

### 1.1 IPC（`src-tauri/src/ipc/mod.rs`，D20）

新增/扩展（serde 默认、向后兼容；同二进制两端，字段沿用既有 snake_case 风格）：

- `ClientMsg::AgentEvent { agent: String, event: String, session_id: String, pid: Option<u32>, cwd: Option<String>, ts: u64 }`：`__agent-hook` 上报；`event ∈ session-start | turn-start | turn-end | session-end`。
- `ClientMsg::AgentsSubscribe`：状态窗口握手后发，之后 daemon 持续推 `AgentsState`。
- `ServerMsg::AgentsState { agents: serde_json::Value }`：一份**全量快照**（agent 列表，已含解析好的标题/状态/字段）。变化时推 + 心跳推（如每 5–10s 一次兜底）。
- `TaskRequest` 增可选字段：`agent_kind: Option<String>`、`agent_session_id: Option<String>`、`agent_pid: Option<u32>`（D21，CLI 顺带填）。
- 是否 bump `PROTOCOL_VERSION`：新增变体对 serde 向后兼容；且不同二进制必触发指纹换新（同二进制两端），故**可不 bump**；如担心旧 daemon 收到新变体的处理，编码时确认 `#[serde(other)]` 或忽略未知即可。

### 1.2 daemon Agent 注册表（`src-tauri/src/daemon/agents.rs`，新文件）

- `AgentRecord { kind, session_id, pid: Option<u32>, title: Option<String>, cwd: Option<String>, started_at, last_activity, state, ended_at: Option<u64> }`；`state ∈ Working | Idle | Ended`。
- `AgentRegistry`（`Mutex` 包裹内部表）：
  - `apply_event(ev)`（D5/D6/D7）：
    - 解析 `session_id` 为身份。若该 `session_id` 未登记 → 新建记录（**幂等登记**，任何事件都能建，不依赖 sessionStart，D6）。
    - **pid 发现**：事件带 pid 则记下；据此做存活轮询。
    - **轮换处理（D7）**：若事件 pid 已存在「另一个 session_id 的活动记录」→ 把旧记录置 `Ended`（移入已结束保留区），新记录用该 pid。
    - 状态推导：`turn-start`→`Working`；`turn-end`→`Idle`；`session-start`→`Idle`（若新建）；`session-end`→`Ended`。
    - 更新 `last_activity`（重置 TTL，D12）。
  - `touch_activity(kind, session_id, pid)`（D21）：仅当该 session 已存在时刷新 `last_activity`（重置 TTL）；不新建。
  - `poll_liveness()`（D5）：对每个有 pid 的活动记录 `kill0`；死亡 → `Ended`（pid 死亡 ⇒ 该 pid 当前活动 session 结束）。
  - `ttl_sweep()`（D12）：对**无 pid / 不可轮询**的活动记录，`now - last_activity > 1h` → `Ended`。
  - `retain_ended()`（D11）：已结束记录全局只留最近 10（按 `ended_at` FIFO）。
  - `snapshot() -> serde_json::Value`：补齐标题（缓存），输出列表（窗口侧再分组排序，或在此排好——见 1.4）。
  - `working_count()`：状态为 `Working` 且 pid 存活的数量（供闲退守卫，D18）。
  - `persist()` / `load()`：原子写 / 读 `~/.askhuman/agents.json`（新增 `paths::agents_file()`）；`load()` 后对每条 `kill-0` 复核、死的置 `Ended`（D18）。

### 1.3 daemon serve 接线（`src-tauri/src/daemon/mod.rs`）

- `ServerState` 增 `agents: Arc<AgentRegistry>`；启动时 `load()` + 复核。
- `control_loop`：新增分支
  - `ClientMsg::AgentEvent(..)` → `agents.apply_event()` + 持久化 + `broadcast_agents_state()`，即时应答（或无应答即可，hook 不等回包）。
  - `ClientMsg::AgentsSubscribe` → 返回 `Control::Subscribe` 进入长驻推送处理器（类似 `handle_gui`，但只下行推 `AgentsState`、不收 answer；连接保持即占一个 `active`，自然阻止闲退）。
- 后台任务：`poll_liveness` + `ttl_sweep` 周期（如每 2–3s）；任一状态变化 → 持久化 + 广播。
- **订阅广播**：参照 `RequestRegistry::broadcast_to_guis` 另起一个「agent 订阅者」发送端列表（`Vec<UnboundedSender<ServerMsg>>` 或专用 registry），变化时推快照。
- **闲退守卫（D18）**：现有空闲检查 `if active==0 { ... }` 改为 `if active==0 && agents.working_count()==0 { ... }`（`active` 已含订阅窗口连接；working agent 额外保活）。空闲 agent **不**计入。
- **drain 不变（D18）**：`begin_drain` / Hello 的 stale 判定仍只看 `registry.active_count()`（在途 ASK 请求），**不**纳入 agent 注册表；agent 状态在换新后由新 daemon `load()` 复活。
- 收尾（shutdown / drain 退出前）：`agents.persist()`。

测试：`AgentRegistry` 全部纯逻辑可单测（apply_event 各事件序列、轮换、TTL、retain、working_count）。

---

## Phase 2：`__agent-hook` 上报器 + `agents status` 窗口

### 2.1 上报器子命令（`cli/mod.rs` dispatch 新分支，D4/D22/D23）

`AskHuman __agent-hook <agent> <event>`：

1. 解析 `agent`(intended) / `event`。
2. 读 stdin JSON（best-effort，限时 / 限长）→ `session_id`（+ `cwd`/`transcript_path` 若有）。
3. `detect_running_agent(env)`：若**明确**识别出 ≠ intended → **`exit 0` 立即返回**（去重 D22）；判不出 → 当作 intended 继续（D22）。
4. `session_id` 兜底：stdin 没有则用 `session_id_from_env`。
5. `walk_agent_pid`：best-effort 找 agent pid（拿不到则 `None`，落 TTL）。
6. `client::ensure_running()` 拉起 daemon（若没在跑）→ 连 socket → 发 `ClientMsg::AgentEvent{..}`（fire-and-forget，整体限时如 2s）。
7. **永远 `exit 0` + 不向 stdout 写任何内容**（D23）。所有错误 fail-open。

> 注意：`ensure_running` 会拉起 daemon——这正是「首个 sessionStart 即拉起 daemon 开始追踪」的预期；与 D18 的闲退相容。

### 2.2 ask 调用顺带上报活动（`client/`，D21）

- `client::run_ask`（提交前）：`detect_running_agent` + `session_id_from_env` + `walk_agent_pid`，把 `agent_kind/agent_session_id/agent_pid` 填进 `TaskRequest`（D20）。daemon 收 `Submit` 时若带这些字段 → `agents.touch_activity()`（刷新最近活动 + 重置 TTL，仅刷新已存在记录）。
- 必须**不阻塞**作答主链路、不改 stdout / 退出码。

### 2.3 `agents` 子命令组（`cli/mod.rs`，D19）

- 新增 `"agents" => crate::agents_cli::dispatch(&argv[2..])`，内部 `match sub { "status" => run_agents_window(), "" | _ => 用法提示 }`。预留扩展位（未来子命令）。
- `run_agents_window()`：与 `--settings`/`--history` 同样在本进程跑 Tauri 窗口（`app::run_agents(...)`）。

### 2.4 状态窗口（GUI）

后端 `app::run_agents`（`src-tauri/src/app/mod.rs` 仿 `run_history`）：

- 创建窗口（`?view=agents`）；窗口内 Rust 侧起一个任务：`client::ensure_running()` → 连 daemon → `Hello` → `AgentsSubscribe` → 循环收 `ServerMsg::AgentsState` → `emit("agents-state", snapshot)` 给前端。
- **断连自动重连**（D18）：连接断开 / 收到 `Draining`/`Restarting` → 退避重试（必要时再 `ensure_running` 拉起新 daemon），重连后重新 `AgentsSubscribe`。
- 命令（`commands.rs`）：`agents_init`（主题 + 语言初值）、`agents_state_initial`（可选：连上前先拉一帧）。

前端：

- `App.vue` 增 `?view=agents` → `AgentsView.vue`。
- `AgentsView.vue`：监听 `agents-state` 事件渲染；**按类型分组**（Claude/Codex/Cursor 区块），区块内**按状态【工作中→空闲→已结束】**、同状态按时间倒序（D13）；每条展示 D9 字段；状态用本地化标签 **工作中 / 空闲 / 已结束**（D8，中文用词；en 对应 Working/Idle/Ended）。
- `lib/types.ts` 增 `AgentSnapshot` 类型；`lib/ipc.ts` 增封装；i18n（D24）。
- 复用既有窗口样式 / 主题 / 毛玻璃。

---

## Phase 3：三家 lifecycle hook 安装 / 卸载 / 状态 + 设置 UI

### 3.1 Hook 集成（`src-tauri/src/integrations/`，D5/D16/D17）

新增 `agent_lifecycle.rs`（或每家一份），对外暴露 `status/install/uninstall/needs_update(agent)`：

- **命令串**：各事件命令 = `"<exe 绝对路径> __agent-hook <agent> <event>"`。marker = 命令含 `__agent-hook`（与 timeout hook 的 `askhuman-timeout.sh` 区分，D16）。
- **Claude**（`~/.claude/settings.json`，jsonc CST 风格，复用 `claude_hook.rs` 的增删手法，D17）：注册 `SessionStart` / `UserPromptSubmit` / `Stop` / `SessionEnd` 各一条命令 hook（matcher 视事件，session/stop 类无 matcher）。**只动**本功能注入的条目，保留其它（含既有 timeout 的 `PreToolUse`）。
- **Cursor**（`~/.cursor/hooks.json`，version 1，jsonc CST，D17）：注册 `sessionStart` / `beforeSubmitPrompt` / `stop` / `sessionEnd`。保留既有 `preToolUse`(timeout) 与其它源。
- **Codex**（`~/.codex/config.toml`，TOML，D17 + 风险见 spec §6）：写 `[hooks]` 下 `SessionStart` / `UserPromptSubmit` / `Stop`（无 sessionEnd），并计算 + 写 `[hooks.state."<key>"] trusted_hash`。
  - **Rust 实现信任哈希**：移植 `demo/agent-lifecycle/harness/codex-trust.cjs`——状态键 `<定义文件绝对路径>:<event_snake>:<group_idx>:<handler_idx>`；哈希 `"sha256:" + sha256_hex(紧凑·键名递归字典序排序 JSON(归一 hook identity))`；identity / handler 归一规则见 `FINDINGS §6.2`。**实现前先对 Codex 源码（`/Users/wutian/Developer/codex`）复核「定义在 config.toml 的 hook，其 hook_key 用的文件路径」**（demo 是 `.codex/hooks.json` 路径；config.toml 路径需确认），并以「装上后 `/hooks` 显示 Active/Trusted + 事件真触发」实测校验。
  - 用 TOML 库做格式保留编辑（只增删本功能键；保留用户其它配置 / 既有 `[projects]`/`[hooks]`）。项目信任沿用仓库根已有 trusted（不新增）。
- `supported()`：仅 unix。

### 3.2 命令层（`commands.rs` + `lib/ipc.ts` + `lib/types.ts`）

- `agent_lifecycle_status(agent) -> { installed, outdated, supported }`、`agent_lifecycle_install(agent)`、`agent_lifecycle_uninstall(agent)`（参数 `agent ∈ claude|codex|cursor`，仿既有 `cursor_hook_*` / `agent_rule_*`）。

### 3.3 配置（`config.rs`，D15）

- 新增 `ExperimentalConfig { enabled: bool=false }`，挂到 `AppConfig.experimental`（serde 默认、向后兼容）。仅此一项持久化；per-agent 状态由安装状态推导（D16），不入 config。

### 3.4 设置 UI（`SettingsView.vue` + i18n，D15/D16/D24）

- 「通用」Tab 底部加一个**隐蔽**开关「实验性功能」（绑 `experimental.enabled`，`save_settings` 持久化）。**Windows 不渲染**（`@tauri-apps/plugin-os` 平台判断 / 既有平台探测）。
- `experimental.enabled` 为真且非 Windows → Tab 栏出现「实验」Tab：
  - 顶部一句风险/说明文案（实验性、用户级 hook、可随时关闭）。
  - 三张卡：Claude Code / Codex / Cursor，各一个「生命周期追踪」开关 + 状态（已安装 / 未安装 / 需更新 / 不支持），开 = `agent_lifecycle_install`、关 = `agent_lifecycle_uninstall`；「需更新」给覆盖按钮（仿既有 hook 卡）。
  - 可放一个「打开 agent 状态窗口」按钮（等价 `agents status`）。
- 关掉「实验性功能」开关**不**卸载 hook（仅隐藏 UI，D16）；如需停止追踪请逐家关闭。

---

## Phase 4：实测 + 文档

- **实测（用户操作，沿用 `FINDINGS` 的低轮次方法论）**：
  - 用户先在设置开启「实验性功能」+ Claude / Codex 两家开关（Cursor 由用户日常开发中验证，D 用户说明）。
  - `AskHuman agents status` 打开窗口；用户启动 / 提问 / 关窗 / `kill -9` claude 与 codex，AI 读窗口快照 + `agents.json` + daemon.log 核对：登记 / 工作中↔空闲 / 已结束（含 kill-9 靠轮询）/ 标题解析 / 跨项目分组 / 排序 / 已结束保留 10 / 闲退与重连 / ask 调用刷新 TTL。
  - Codex 安装后 `/hooks` 应显示 lifecycle hook Active/Trusted（校验信任哈希）。
- `./scripts/install.sh` 编译进环境，按 AGENTS.md 用新装的 `AskHuman` 继续验证。
- `cargo test` / `npm run build` 全绿。
- 更新 `docs/overview.md`（新增 agent-tracking 模块 / 命令 / 窗口）与 `docs/PROGRESS.md`。
- 提交按 Conventional Commits（如 `feat(agents): ...`）。

---

## 补丁（2026-07-04）：Codex app-server 共享 pid 隔离（spec D25/D26/D27 + §8）

> 背景/结论见 spec §8：新版 Codex TUI 经 UDS 连长寿共享 app-server，walk 永远拿不到 TUI pid，只会命中 app-server（reparent 到 PID 1、多会话共用）。目标：**让共享 app-server pid 不进入会话身份/存活**，Codex 该类会话落到既有「无 pid」路径（与 Claude 被 scrub 时同一路径）。**不新增状态字段、不改状态机**——只在 detect 层把「app-server pid」归一成 `None`。

### P-1 `agents/detect.rs`：识别共享 app-server → 记 None

- 新增 `fn is_shared_app_server(entry: &ProcEntry) -> bool`（判据 D27，纯函数、可单测）：
  - **主判据**：命令行 whitespace 分词后，找到 basename=`codex` 的 token，**跳过其后前导全局选项**（`-c`/`--config` 及其值、`--flag=value`、其它 `-…` flag），第一个非选项 token 为 `app-server`（覆盖 `codex app-server …`、`node …/codex app-server …`、ChatGPT Desktop 的 `codex -c features.…=true app-server …`）。**不是**「任意位置出现 `app-server` 令牌」。
  - **可选兜底**：`entry` 无 tty（`ps -o tty=` 为空/`??`）**且** 父链上溯到 PID 1（`process_chain` 末端 ppid==1）。默认可只用主判据（更专一、少一次 `ps`）；是否叠加兜底见评审结论。
- `walk_agent_pid(kind, start_pid)`：命中的 Codex 祖先若 `is_shared_app_server` → 返回 `None`（该会话无可用 pid）。**仅对 Codex 生效**（其它家族不变）。
- `walk_any_agent(start_pid)`（MCP 兜底）：跳过 `is_shared_app_server` 的节点（继续上溯）；若最终只剩 app-server → 返回 `None`（不按共享 pid 做 `touch_activity_by_pid`，规避跨 session 串味）。
- `terminal_kind` 不动：pid=None 时上层本就不查，Codex 会话「聚焦终端」按钮恒隐藏（可接受，spec 风险已记）。

### P-2 `agents/registry.rs`：无改动（复用既有无 pid 路径）

- pid=None 时：`apply_event` 的 D7 轮换 `if let Some(pid)` 天然跳过；`poll_liveness` 只扫 `pid.is_some()`；`ttl_sweep` 只扫 `pid.is_none()`；`working_backstop_sweep` 与 pid 无关照常生效。**即 D25 所说「同 Claude 无 pid 路径」，无需改注册表逻辑**。
- 仅补充/修订模块头与相关方法的文档注释，点明「Codex 共享 app-server 场景由 detect 归一为无 pid」。

### P-3 测试

- `detect.rs` 单测：`is_shared_app_server` 命中 `codex app-server --listen unix://`/`stdio://`、Desktop `codex -c features.…=true app-server`；不命中纯 `codex`（TUI）、`codex exec … app-server …`、argv 里恰好含 "app-server" 但 basename 非 codex 的进程；`walk_agent_pid(Codex, …)` 对构造链（app-server 祖先）返回 `None`。
- 复用既有 registry 无 pid 用例（`ttl_only_affects_pidless_records` / `working_backstop_*`）即覆盖生命周期治理，无需新增注册表测试。

### P-4 验证

- `cargo test`（新增 detect 单测 + 既有全绿）。
- `./scripts/install.sh` 编译进环境；按 AGENTS.md 用新 `AskHuman`：真机对一个 app-server 模式的 Codex 会话触发一次工具/提问，核对 `agents.json` 里该会话 `pid=null`、状态随 hook（Stop→空闲）与兜底超时正确流转、不再出现并发会话互相轮换误杀。

## 关键风险与对策（汇总）

| 风险 | 对策 |
|---|---|
| Codex 信任哈希随 Codex 版本失效（spec §6） | Rust 复刻算法前先核对源码；`status` 能识别「未受信任/漂移」并提示重装；实测以 `/hooks` Active 为准 |
| Linux Claude PID namespace 隔离导致 walk/kill-0 失效 | 落 D12 的 1h TTL 兜底；ask 调用与任意事件刷新活动 |
| 双触发（Cursor 兼容加载 `~/.claude`） | `__agent-hook` 内 `detect_running_agent` 去重（D22），仅 running==intended 才上报 |
| daemon 闲退丢失追踪 | 持久化 agents.json + 重启复核（D18）；working agent / 窗口连接保活；drain 不受影响 |
| 写配置破坏其它 hook | 一律走格式保留编辑、只动自己条目（D17），加幂等 / 保留性单测（仿 `claude_hook.rs` 测试） |
| hook 频繁拉起进程的开销 | 仅装 session/turn 四类（不装工具级 D5）；上报器轻量、限时、fire-and-forget |
