# 开发计划：按 Agent 分组的「全局提示词（Rules）」配置入口

> Windows 注（2026-08）：本文保留首期 Cursor Hook 限制；当前 Windows Cursor/Claude timeout hooks
> 使用 PowerShell 5 兼容 `.ps1`，Codex 使用 `commandWindows`，旧占位/unsupported 描述仅作历史记录。

> 关联需求：`docs/specs/agent-rules-config.md`
> 计划描述方案与技术/规则细节，具体代码以实现为准。

## 0. 方案总览

```
设置页 Agent Tab (SettingsView.vue)
  ┌ 参考提示词卡：cli_reference() + 复制（文案改为「加入 Agent 的 Rules」）
  ├ Cursor 组
  │    Rules → 安装/卸载/定位   ── invoke ──▶ agent_rule_* (Rust)
  │    Hook  → 现有 cursor_hook_* 不变
  ├ Claude Code 组
  │    Rules → 安装/卸载/定位   ── invoke ──▶ agent_rule_*
  │    Hook  → 占位「即将支持」
  └ Codex 组
       Rules → 安装/卸载/定位   ── invoke ──▶ agent_rule_*
       Hook  → 占位「即将支持」

Rust 后端
  commands.rs: agent_rule_status / install / uninstall / reveal (按 agent 入参)
       │
       ├ integrations/agent_rules.rs（新增）
       │     ├ managed_block: upsert_block / remove_block / has_block（纯函数，单测）
       │     ├ Cursor 独占文件：build_cursor_rule / install / uninstall / status
       │     └ Claude·Codex 共享文件：install / uninstall / status（复用 managed_block）
       ├ prompts::cli_reference()  —— 三者共用的提示词正文
       └ paths.rs: 新增 claude/codex/cursor-rules 路径助手
```

要点：所有「改文件」的核心逻辑都做成**纯函数**（输入旧文本/输出新文本），IO 与平台相关「打开/定位」单独封装，便于单测与跨平台。

---

## 1. 标记与内容格式（务必逐字一致）

### 1.1 托管区块（Claude Code / Codex，写入共享文件）
```
<!-- AskHuman:begin DO NOT EDIT (managed by AskHuman) -->
<cli_reference() 完整正文>
<!-- AskHuman:end -->
```
- `upsert_block(text, body)`：用 `(?s)` 正则匹配 `begin…end` 整段；命中→替换为「新 begin/body/end」；未命中→在 `text` 末尾追加（若原文非空且不以空行结尾，先补一个空行）。
- `remove_block(text)`：删除 `begin…end` 整段；清理因此产生的多余连续空行；返回结果（trim 尾部多余换行，保留单个结尾换行）。
- `has_block(text)`：是否存在 `begin` 标记。

### 1.2 独占文件（Cursor，整文件由本应用拥有）
`~/.cursor/rules/askhuman.mdc` 内容固定为：
```
---
alwaysApply: true
---
<!-- AskHuman:managed-file DO NOT EDIT (managed by AskHuman) -->

<cli_reference() 完整正文>
```
- `build_cursor_rule(body)`：拼出上面整段。
- 识别：文件文本是否包含 `AskHuman:managed-file`。

> 说明：标记是 Markdown 注释，`.mdc`/`.md`/`AGENTS.md` 中都不渲染；模型上下文里只会看到一行简短的「managed by AskHuman」说明，无副作用。

---

## 2. Rust 后端

### 2.1 `paths.rs` 新增助手
- `claude_dir()` → `~/.claude`，`claude_md()` → `~/.claude/CLAUDE.md`
- `codex_dir()` → `~/.codex`，`codex_agents_md()` → `~/.codex/AGENTS.md`
- `cursor_rules_dir()` → `~/.cursor/rules`，`cursor_rule_file()` → `~/.cursor/rules/askhuman.mdc`
（沿用现有 `home()` 与 `cursor_dir()`。）

### 2.2 新增模块 `integrations/agent_rules.rs`
- **Agent 标识**：内部用一个枚举 `AgentTarget { Cursor, ClaudeCode, Codex }`；命令层用字符串 `"cursor" | "claude" | "codex"` 解析为枚举（未知值返回错误）。
- **常量**：`BLOCK_BEGIN` / `BLOCK_END` / `MANAGED_FILE_MARK` 三个标记字符串。
- **纯函数（单测覆盖）**：`upsert_block` / `remove_block` / `has_block` / `build_cursor_rule` / `is_managed_cursor_file`。
- **IO（每 Agent 一组）**：
  - `status(agent) -> RuleStatus`：读取目标文件判断是否已安装；返回 `{ installed, path(展示用，做 ~ 缩写), supported }`。
  - `install(agent)`：
    - Cursor：`create_dir_all(~/.cursor/rules)` → 原子写 `askhuman.mdc = build_cursor_rule(cli_reference())`。
    - Claude/Codex：读旧文本（不存在按空串）→ `upsert_block(old, cli_reference())` → 原子写回（必要时 `create_dir_all` 父目录）。
    - 返回可展示的成功文案（走 i18n）。
  - `uninstall(agent)`：
    - Cursor：仅当文件含 `MANAGED_FILE_MARK` 时删除文件（否则不动并返回提示）。
    - Claude/Codex：读旧文本 → `remove_block` → 写回（若结果为空可保留空文件，不必删共享文件）。
  - `reveal(agent)` / `open(agent)`：定位 / 打开目标文件（见 §2.4）。
- **原子写**：复用 `cursor_hook.rs` 里 `atomic_write`（写临时文件 + rename）的同款做法（可抽到一个公共小工具或各自保留，按实现简洁度定）。

### 2.3 `models.rs`
- 新增 `RuleStatus { installed: bool, path: String, supported: bool }`（`serde` 序列化给前端）。

### 2.4 跨平台「打开 / 定位」
- 复用并推广 `cursor_hook::reveal` 的平台分支：
  - **定位(reveal)**：mac `open -R <file>`；linux `xdg-open <父目录>`；win `explorer /select,<file>`。
  - **打开(open)**：mac `open <file>`；linux `xdg-open <file>`；win `explorer <file>`（或 `cmd /c start`）。
- Cursor/Claude/Codex 的 Rules 文件读写**均跨平台**（用 `paths::home()`，Windows 自然解析到 `%USERPROFILE%`）。
- **Cursor Hook 维持现状仅 unix**（写 bash 脚本），不在本次改动范围。

### 2.5 `commands.rs` + 注册
- 新增命令（按 agent 入参）：`agent_rule_status(agent: String)`、`agent_rule_install(agent)`、`agent_rule_uninstall(agent)`、`agent_rule_reveal(agent)`、`agent_rule_open(agent)`。
- 在 `app/mod.rs`（或现有 `generate_handler!` 处）注册上述命令。
- Cursor Hook 的既有命令（`cursor_hook_status/install/uninstall/reveal`）保持不变。

---

## 3. 前端

### 3.1 `lib/types.ts`
- 新增 `RuleStatus` 类型；`AgentId = "cursor" | "claude" | "codex"`。

### 3.2 `lib/ipc.ts`
- 新增 `agentRuleStatus(agent)`、`agentRuleInstall(agent)`、`agentRuleUninstall(agent)`、`agentRuleReveal(agent)`、`agentRuleOpen(agent)`。

### 3.3 `views/SettingsView.vue`（重做原「集成」Tab）
- Tab 文案改用 i18n `settings.tabs.agent`（值固定 `Agent`）。内部 `Tab` 类型把 `"integration"` 改名为 `"agent"`（或保留键名仅换显示文案，二选一以最小改动为准）。
- **顶部卡**：保留「参考提示词」展示 + 复制；改用新的 `promptDesc` 文案。
- **三组 Agent 卡**（紧凑布局）：每组一张卡，卡内：
  - 组标题（Cursor / Claude Code / Codex）。
  - **Rules 行**：状态徽标（已安装/未安装，复用现有 `.badge`/`.dot`）；未装→「安装」按钮；已装→「卸载」+「定位」（必要时加「打开」）。Cursor 行下方加小字 hint（项目需在 home 目录下）。
  - **Hook 行**：Cursor 用现有 hook 逻辑（安装/卸载 + 打开 hooks.json，含 Windows 不支持提示）；Claude/Codex 显示「即将支持」占位（禁用态）。
- **加载时**：`onMounted` 并发拉三个 `agentRuleStatus` + 现有 `cursorHookStatus`，填充状态。
- **紧凑样式**：复用现有 `.card`/`.row`/`.badge`，但收紧 Agent 组的内边距/行距（新增局部 scoped class，压缩留白）。

### 3.4 i18n（`src/i18n/zh.ts` + `en.ts`）
- `settings.tabs`：把 `integration` 显示改为 `Agent`（新增 `agent: "Agent"`，两语一致）。
- `settings.integration.promptDesc` 改为：
  - zh：`复制下面的提示词，添加到你的 Agent 的 Rules 中，引导它通过 AskHuman 与你交互。`
  - en：`Copy the prompt below and add it to your Agent's rules to guide it to interact with you via AskHuman.`
- 新增键（建议归到 `settings.agent.*`，与现有 `integration.*` 共存或迁移）：
  - 组标题：`cursorTitle`/`claudeTitle`/`codexTitle`（Cursor / Claude Code / Codex）。
  - Rules 子项：`rulesTitle`、`rulesDesc`（各 Agent 文件路径说明）、`install`/`uninstall`/`reveal`/`open`、`installed`/`notInstalled`。
  - Cursor 作用范围小字：`cursorHomeHint`。
  - Hook 占位：`hookComingSoon`（「即将支持」/ `Coming soon`）。
  - 复用现有 `installed`/`notInstalled`/`install`/`uninstall` 等可不重复新增。

---

## 4. 验证与收尾

- **单测**：`cargo test --manifest-path src-tauri/Cargo.toml` 覆盖 `upsert_block`（追加/替换/幂等/保留他人内容）、`remove_block`（删段/清空行/不误删）、`build_cursor_rule` 与 `is_managed_cursor_file`（识别/防误删）。
- **编译安装**：`./scripts/install.sh`，随后用新装的 `AskHuman --settings` 实测三组安装/卸载/定位与状态判定。
- **文档**：更新 `docs/overview.md`（集成→Agent 页、新模块/命令/路径助手），并清理 `docs/PROGRESS.md` 的本任务标记。

## 5. 里程碑

- **M1 后端纯函数 + 路径 + 模块**：`agent_rules.rs`（标记常量、upsert/remove/build + 单测）、`paths.rs` 助手。
- **M2 命令 + IPC**：`commands.rs` 五个命令 + 注册；`lib/ipc.ts` 封装。
- **M3 前端重做**：Agent Tab 分组 UI + 紧凑样式 + 状态拉取 + 按钮接线。
- **M4 i18n + 文案**：Tab=Agent、promptDesc、各新键中英补齐。
- **M5 收尾**：install.sh 编译实测、单测过、更新 overview/PROGRESS。

## 6. 风险

- **Cursor 全局规则非官方承诺**：依赖「项目在 home 之下 + `.cursor/rules/**/*.mdc`」的现有行为；已用 UI 小字提示作用范围。若未来 Cursor 改变加载规则需同步调整（低概率、可控）。
- **共享文件并发写**：CLAUDE.md / AGENTS.md 可能被用户/其它工具同时编辑；用「读-改-原子写」最小化窗口，区块标记保证幂等，不做文件锁（可接受）。
- **Windows 打开/定位差异**：`explorer /select` 与 `xdg-open` 行为差异已在 §2.4 分支处理；Cursor Hook 仍仅 unix 不变。
