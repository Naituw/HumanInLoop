# Grok 集成实现计划（仅 MCP 方案）

> Windows 注（2026-08）：本文保留首期 hook 平台假设；Windows Grok binding/MCP/lifecycle 已由平台
> 对齐项目接入 shared daemon，以下 Unix-only 描述仅作历史记录。

> 需求/调研背景：`docs/specs/grok-cli-integration-research.md`（Grok CLI 0.2.82 实测：harness 差异、rules
> 注入、MCP per-tool 超时、hook 兼容读取等证据链）。本计划只覆盖**实现方案**。
> 状态：草案（Q1–Q5 已定；第 6 节「Claude/Cursor 兼容读取的坑」P1/P2/P3 已讨论定案，见 §6.2）。

## 1. 目标

把 Grok CLI 作为**第 4 个可自动集成的 Agent**接入 AskHuman，与现有 Cursor / Claude Code / Codex 同级
（设置页卡片 + CLI `agents`/`doctor` + 产物过期检测/更新）。**只支持 MCP 方案**（不做 CLI/shell 方案）。

为何不做 CLI 方案（调研已证，写入依据）：Grok 两个模型的终端工具行为不同——Grok Build 用 `bash`
toolset（前台超时 `[toolset.bash] timeout_secs` 默认 120s，可调大）；而**默认模型 Composer(cursor
harness) 的 `run_terminal_command` 命中默认超时会「自动移到后台」并回「仍在运行」**，阻塞式 CLI 提问会被
放弃、agent 继续执行 → 破坏 AskHuman 的阻塞语义，且无文档化配置可延长。故 Grok 只走 MCP。

## 2. 决策结论（本轮已确认）

| 编号 | 决策 | 结论 |
|---|---|---|
| Q1 | 指令载体 | **只装 Skill**（`~/.grok/skills/askhuman/SKILL.md`），**不写 `~/.grok/AGENTS.md` 全局 rules**。原因：默认模型 Composer 不读 `~/.grok/AGENTS.md`（调研证实），skill 是唯一能同时覆盖 Composer 与 Grok Build 的入口。 |
| Q2 | 模式态 | Grok 只提供 **None \| Mcp** 两态（不提供 Cli 档）。 |
| Q3 | 生命周期追踪 | **本轮一并做** Grok 生命周期追踪（原生 `~/.grok/hooks`）。附带专门处理「Grok 兼容读取 Claude/Cursor hook 导致错标/重复」的坑（见 §6）。 |
| Q4 | MCP 超时键 | `~/.grok/config.toml` 的 `[mcp_servers.askhuman]` 写 `startup_timeout_sec=30` + `tool_timeout_sec=86400` + `tool_timeouts = { ask = 86400 }`（对 Composer 的 per-tool 语义更精准）。 |
| Q5 | 入口范围 | **全套**：GUI 设置页卡片 + CLI（`agents mode/show/install/uninstall/update`、`doctor`），与现有三家一致。 |

## 3. 产物与落点（Grok / MCP 模式）

| 产物 | 落点 | 机制 | 说明 |
|---|---|---|---|
| MCP 配置 | `~/.grok/config.toml` `[mcp_servers.askhuman]` | `toml_edit` 最小编辑（复用 `mcp_config.rs` 的 Codex 范式） | `command`=当前 exe 绝对路径，`args=["mcp"]`，三个超时键。 |
| Skill（指令） | `~/.grok/skills/askhuman/SKILL.md` | 新增 `grok_skill.rs`：整文件拥有（AskHuman 独占该目录），幂等写/删 | 内含 AskHuman 交互协议 + 两 harness 如何调用 `ask` MCP 工具。 |
| 生命周期 hook | `~/.grok/hooks/askhuman-lifecycle.json` | 复用 `agent_lifecycle.rs` 的 JSON 嵌套范式（原生 Grok 事件） | 仅在开启「生命周期追踪」时安装；全局 `~/.grok/hooks` 恒受信任，无需信任哈希。 |

三态编排：Grok 的 `Mcp` 模式 = **Skill + MCP 配置**（无 Rule、无 timeout Hook）。生命周期 hook 与三态
正交（独立实验开关），同现有三家。

## 4. 详细方案

### 4.1 MCP 配置（`integrations/mcp_config.rs`）

- 在 `AgentTarget` 增加 `Grok`（该枚举定义在 `agent_rules.rs`，被 `mcp_config.rs` 复用）。
  - `AgentTarget::parse("grok") => Grok`；`format_of(Grok) = Toml`；`config_path(Grok) = paths::grok_config_toml()`（=`~/.grok/config.toml`）。
- TOML 写入沿用 `apply_install_toml`，但 Grok 需**额外写 `tool_timeouts.ask`**（Codex 目前只写
  `tool_timeout_sec`）。做法：在 entry 上再 upsert 一个内联表/子键 `tool_timeouts = { ask = 86400 }`
  （`toml_edit` inline table），并让 `toml_entry_matches`/`needs_update` 对 Grok 校验该键。
  - 抽出与目标相关的「超时字段集」，Codex=`{startup,tool}`、Grok=`{startup,tool,tool_timeouts.ask}`，避免
    分叉两套写入逻辑。
- 常量：`GROK_STARTUP_TIMEOUT_SEC=30`、`GROK_TOOL_TIMEOUT_SEC=86400`、`GROK_ASK_TOOL_TIMEOUT_SEC=86400`
  （或复用 Codex 的 86400 常量 + 新增 ask 子键常量）。
- `supported(Grok)=true`（跨平台，同 Codex）。

### 4.2 指令载体：Skill（`integrations/grok_skill.rs`，新模块）

- 落点 `~/.grok/skills/askhuman/SKILL.md`（AskHuman 独占该 skill 目录），装/更新/卸载/状态/needs_update/
  reveal/open，纯函数 + 单测，风格对齐 `agent_rules.rs`。
- 文件格式（Grok skill 规范）：YAML frontmatter（`name`、`description`）+ 正文。
  - `name`: `askhuman`；`description`: 含强触发词（AskHuman、human input、question、clarification、
    approval、review、wait for user…），提高模型「首次需要提问前」加载概率。
  - 正文 = AskHuman 交互协议（复用 `prompts::mcp_reference()` 的纪律）+ **Grok 两 harness 的 `ask`
    工具调用指引**（Composer：经 `CallMcpTool` 调用 AskHuman 的 `ask`；Grok Build：先 `search_tool`
    找到 AskHuman 的 `ask` 再 `use_tool` 调用）。新增 `prompts::grok_skill_body()` 或在 `mcp_reference`
    基础上补 harness 段落。
  - **优先级措辞（P2 定案，务必写准）**：只针对「调用 AskHuman 的 `ask`」这一件事声明「**MCP 工具优先于
    shell/命令行方式**」；**不**禁止一般的 shell 调用（其它 shell 命令照常可用）。即：`ask` 走 MCP，其余不限。
    目的：即便 Grok Build 经 `[compat.claude] agents` 读到他家「shell 版 AskHuman 协议」，本 skill 也能把
    `ask` 这一动作拉回 MCP，而不误伤用户正常的 shell 用法。
- 约束边界（写入文档/UI 文案，不夸大）：skill 属**弱约束**——模型需先判定相关才加载，不像全局 rules
  每轮强制；Grok 升级后需回归「首次澄清是否主动加载 skill」。

### 4.3 二态模式（`integrations/agent_mode.rs` + 前端）

- 现有 `agent_mode` 假设 `Cli=Rule(CLI)+Hook`、`Mcp=Rule(MCP)+MCP`。Grok 需特化：
  - Grok 只有 `None | Mcp`；`Mcp` 产物 = **Skill + MCP 配置**（不含 Rule/Hook）。
  - `current`/`needs_update`/`set`/`update`/`uninstall_all` 对 Grok 走「skill + mcp」这套产物判定。
- 前端 Agent Tab 增加 Grok 卡片：分段控件只有 **未集成 | MCP** 两档（MCP 带「推荐」标记）；产物行列
  Skill 与 MCP 配置（各带「打开/在 Finder 显示」+ 过期时「更新」按钮）；纳入跨家「待更新总览」。

### 4.4 生命周期追踪（`agents/*` + `integrations/agent_lifecycle.rs`）

- `AgentKind` 增加 `Grok`（跨文件改动：`agents/mod.rs`、`detect.rs`、`registry.rs`、`title.rs`、
  `report.rs`、`agent_lifecycle.rs`、前端 `AgentsView` 分组/本地化名）。
- 原生 hook 落点 `~/.grok/hooks/askhuman-lifecycle.json`（嵌套 JSON，同 Claude Shape）。事件表（Grok 事件
  最全，含 StopFailure + SessionEnd）：
  - `SessionStart`→session-start、`UserPromptSubmit`→turn-start、`PreToolUse`/`PostToolUse`→activity、
    `Stop`→turn-end、`StopFailure`→turn-end、`SessionEnd`→session-end。
  - 全局 `~/.grok/hooks` 恒受信任（无需 Codex 那种 `[hooks.state]` 信任哈希）。
- 检测（`detect.rs`）：
  - `matches_agent(Grok)`：comm/argv0 命中 `grok`（真实二进制名 `grok-macos-aarch64`，符号链接 `grok`）。
  - `session_id_env_var(Grok) = "GROK_SESSION_ID"`；stdin JSON 兜底 `sessionId`（已在 resolve 列表）。
  - `detect_running_agent_from`：**新增 Grok 分支，且置于最高优先级**——grok 会话的 hook 子进程有
    `GROK_SESSION_ID`，同时 grok 也设 `CLAUDE_PROJECT_DIR` 别名；必须先判 grok，避免落到 claude/cursor。
  - `walk_any_agent` 的 KINDS 加入 Grok。
- 去重（`report.rs`，**这是 §6 坑的核心修复**）：现仅「intended=Claude 且 running=Cursor 跳过」。
  新增：**`running==Some(Grok) && intended!=Grok` → 跳过**（grok 兼容触发了 claude/cursor 的 hook，但
  真实家族是 grok；grok 原生 hook `__agent-hook grok` 则 running==intended==Grok，正常上报）。不改动既有
  codex/cursor 自身上报语义。

### 4.5 入口（CLI + doctor + paths + i18n）

- `paths.rs`：新增 `grok_config_toml()`（`~/.grok/config.toml`）、`grok_skill_md()`
  （`~/.grok/skills/askhuman/SKILL.md`）、`grok_hooks_dir()`（`~/.grok/hooks/`）。
- CLI `agents_cmd.rs`：`agent ∈ cursor|claude|codex|grok`；`mode grok [none|mcp]`、`show grok`、
  `install/uninstall/update grok`（Grok 的 `--mcp` 写 MCP 配置 + skill；无 `--rules/--hook`）。
- `doctor.rs`：体检增列 Grok（mode + skill + mcp + lifecycle 装没装/需更新）。
- i18n：新增 Grok 相关文案键（家族显示名「Grok」、skill 产物名、各提示）。

## 5. 影响面（改动清单，便于评审）

- 新增：`integrations/grok_skill.rs`、`prompts::grok_skill_body()`、`paths` 三个路径、i18n 文案。
- 改动：`agent_rules.rs`（`AgentTarget::Grok`）、`mcp_config.rs`（Grok 的 tool_timeouts.ask + 校验）、
  `agent_mode.rs`（Grok 两态、产物=skill+mcp）、`agents/*`（`AgentKind::Grok` 全链路）、
  `agent_lifecycle.rs`（Grok 原生 hook + events + any_installed/migrate 数组）、`report.rs`/`detect.rs`
  （检测 + 去重）、CLI `agents_cmd.rs`/`doctor.rs`、`commands.rs`（前端命令 agent 参数放行 grok）、
  前端 `SettingsView.vue`（Grok 卡片、两态控件）、`AgentsView.vue`（Grok 分组/名）、`lib/types.ts`。
- 平台：MCP 配置 + skill 跨平台；生命周期 hook 仅 Unix（同现有）。

## 6. 开放分析：Grok 兼容读取 Claude/Cursor 的坑（结论待讨论后补充）

> 依据 `~/.grok/docs/user-guide/10-hooks.md` §Hook Locations 与 05-configuration.md §Harness
> Compatibility，以及 `grok inspect` 实测（本机 grok 加载了 `~/.claude/Claude.md` 与 8 条 `[claude]`
> command hook）。

**事实**：Grok 默认**合并读取**三处 hook —— `~/.grok/hooks/*.json` + `~/.claude/settings.json` +
`~/.cursor/hooks.json`（claude/cursor 兼容，可用 `[compat.claude] hooks=false` / `[compat.cursor]
hooks=false` 或对应 env 关闭）。rules/agents 同理：Grok Build 会读 `~/.claude/CLAUDE.md`（`[compat.claude]
agents`）。

**由此产生的两类问题**：

1. **生命周期 hook 错标/重复（本方案会触及）**：若用户**同时**为真 Claude Code / Cursor 装了 AskHuman
   lifecycle hook，则**跑 Grok 时 grok 也会触发这些 claude/cursor hook** → reporter 收到
   `__agent-hook claude|cursor …`，而真实家族是 grok。
   - **拟处置（在我们掌控内）**：§4.4 的 reporter 去重——检测到 `running==Grok` 时，凡 `intended!=Grok`
     一律跳过；只认 grok 原生 hook。这样 grok 会话只登记为 grok，不再错标为 claude/cursor。**无需**改用户
     的 `[compat.*]`。
   - 待确认：是否足够？是否还要在安装 Grok lifecycle 时提示用户「若已集成 Claude/Cursor，本修复已自动去重」。

2. **指令/rules 交叉污染（超出本方案，但需决策）**：若用户已用 **CLI 模式**集成 Claude（`~/.claude/CLAUDE.md`
   写了 shell 版 AskHuman 协议），Grok Build 会读到它 → 被告知「用 Shell 调 AskHuman、设 24h 超时」，与我们
   给 Grok 的 skill（用 `ask` MCP 工具）**冲突**，且 grok Build 的 bash 超时仅 120s。
   - 候选处置（**待你拍板，不擅自决定**）：
     - (a) 不干预，仅在文档/UI 说明该情形；靠 skill 自身表述让模型优先走 MCP。
     - (b) 安装 Grok MCP 时，主动在 `~/.grok/config.toml` 写 `[compat.claude] agents=false` /
       `[compat.cursor] rules=false` 等，阻止 Grok 读取他家指令——但这会影响用户 Grok 的其它兼容用法，
       侵入性较大。
     - (c) 仅提供一个可选开关/提示，由用户决定是否关闭兼容读取。

### 6.2 最终处置（已定案）

- **P1 生命周期错标 → 只用 reporter 去重（不动用户 `[compat.*]`）**：`report.rs` 检测到
  `running==Some(Grok)` 时，凡 `intended!=Grok` 一律跳过；grok 原生 hook（`__agent-hook grok …`）
  running==intended==Grok，正常上报。与 Claude/Cursor 集成并存时，grok 会话只登记为 grok，不错标。
  不修改用户的兼容开关（精准、零副作用）。
- **P2 指令交叉污染 → (a) 不改用户配置，强化 skill 正文**：skill 只就「**调用 AskHuman 的 `ask`**」声明
  「MCP 工具优先于 shell/命令行」；**不禁止一般 shell 调用**（其它 shell 命令照常）。这样即使 Grok Build
  经 `[compat.claude] agents` 读到他家 shell 版协议，也能把 `ask` 拉回 MCP，而不误伤正常 shell 用法。不写
  `[compat.*]=false`（避免侵入用户其它 Grok 兼容用法）。文档补充该情形说明。
- **P3 MCP 来源重复 → 视为无害，仅文档记一笔**：用户若已把 askhuman 写进 `~/.claude.json`/`~/.cursor/mcp.json`，
  Grok 经 compat 会读到同名 `askhuman` server，与 `~/.grok/config.toml` 里的重复；grok 按 server 名去重，
  同名同 `command` 无实质冲突。不做特殊检测/提示，仅在集成文档记一笔。

## 7. 测试 / 验证

- 单测：`mcp_config`（Grok TOML：含 tool_timeouts.ask 的安装/幂等/更新/卸载/保留他人内容/needs_update）、
  `grok_skill`（frontmatter + 区块幂等/卸载）、`agent_mode`（Grok 两态编排）、`report`（grok 去重矩阵：
  grok 原生上报通过；grok 触发的 claude/cursor 上报被跳过；真 claude/cursor 不受影响）、`detect`
  （GROK_SESSION_ID 优先于 CLAUDE_PROJECT_DIR）。
- 端到端（按 AGENTS.md：改完 `./scripts/install.sh` 后用新 `AskHuman` 验证）：
  - `agents mode grok mcp` → 检查 `~/.grok/config.toml` 与 skill 落盘正确、`doctor` 显示齐全。
  - 真机跑 `grok`（Composer 与 Grok Build 各一次）确认能调用 `ask`、长等待不被超时中断。
  - 开启 Grok 生命周期后，Composer/Build 会话在状态窗口正确显示为 Grok；与 Claude/Cursor 集成并存时不错标。

## 8. 分阶段

1. **P1 MCP 集成（核心）**：`AgentTarget::Grok` + `mcp_config`（含 tool_timeouts.ask）+ `grok_skill` +
   `agent_mode` 两态 + paths/i18n + CLI/doctor + GUI 卡片。可独立交付「Grok MCP 可用」。
2. **P2 生命周期追踪**：`AgentKind::Grok` 全链路 + 原生 hook + detect/report 去重 + 状态窗口。
3. **§6 坑的落实**：P1 去重随 P2 阶段的 `report.rs` 一并实现；P2 skill 措辞随 P1 阶段的 `grok_skill`
   一并实现；P3 只是文档一笔。无需独立阶段。
