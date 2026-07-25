# 开发计划：MCP 模式支持（Codex / Claude Code / Cursor）

> 关联需求：`docs/specs/mcp.md`
> 计划描述方案与技术 / 规则细节，具体代码以实现为准。
> **注（2026-07-25）**：本计划中 `ask` 的输出形态（`--output json` → `AskResult` → structuredContent +
> output schema）为首轮方案，已被 spec mcp.md D5 二轮定案推翻——现为**纯文本区块透传**（无
> output schema / structuredContent），图片路径改从 `[files]` 区块解析。下文相关段落仅作历史记录。

## 0. 方案总览

```
客户端按配置 spawn: <AskHuman 绝对路径> mcp                （STDIO，session 期常驻）
  └─ cli/mod.rs dispatch 新增分支 "mcp" → crate::mcp::run()
       └─ rmcp STDIO server，单工具 `ask`
            ask(入参) ──①Schema→argv──▶ spawn <AskHuman 绝对路径> <argv...> --output json
                          （Unix=瘦客户端→daemon；Windows=单进程弹窗回退；排空/重连全复用）
                       ◀──②捕获 stdout(JSON) + exit code
            ──③解析 JSON，从 answers[].files 按扩展名取图片路径──▶ 读文件→base64
            ──④CallToolResult: structuredContent=JSON + content[ TextContent(序列化JSON) + ImageContent(每张图) ]──▶ 返回模型
```

核心：**MCP 路径不碰 daemon IPC**，全部经「spawn 现有 CLI 子进程」复用。新增代码集中在：①`mcp` 子命令与 server；②MCP 版提示词；③MCP 配置集成 + 模式编排；④设置 UI 三态。

---

## 1. 依赖（`src-tauri/Cargo.toml`）

- 新增 `rmcp`（官方 Rust MCP SDK）：`rmcp = { version = "0.16", features = ["server", "transport-io", "macros"] }`（确切 feature 名实现期对照 docs.rs 校验；需要 `server` + STDIO server transport + `#[tool]`/`#[tool_router]`/`#[tool_handler]` 宏）。
- 已有 `tokio`/`serde`/`serde_json`/`toml_edit`/`jsonc-parser`/`uuid` 复用。

## 2. 新增子命令 `AskHuman mcp`（`cli/mod.rs` + 新模块 `src-tauri/src/mcp/`）

- `cli/mod.rs::dispatch` 在 `match argv[1]` 增分支：
  ```rust
  "mcp" => crate::mcp::run(),   // 进入 STDIO server 事件循环（-> !）
  ```
  放在 `daemon`/`--popup`/`__agent-hook` 同级；**全平台**（不 `#[cfg(unix)]` 门控，平台差异交给子进程）。
- 新模块 `src-tauri/src/mcp/mod.rs`：
  - `run() -> !`：构建当前线程 tokio runtime（同 `client::run_ask` 风格），`block_on` 跑 rmcp server，结束即 `exit`。
  - server：`#[tool_router]` + `#[tool_handler] impl ServerHandler`，`get_info` 写 server 名 `askhuman`、版本、`instructions`（简述用途）。
  - `main.rs` 声明 `mod mcp;`。
- `help.rs`：`--help` 的「管理」块补一行 `mcp`（简述「以 MCP(STDIO) 暴露 ask 工具」）。`mcp` 是面向客户端而非人，help 简述即可。

## 3. `ask` 工具实现（`src-tauri/src/mcp/ask.rs`）

### 3.1 入参 Schema（`schemars` 派生，rmcp `Parameters<T>`）

```rust
struct AskParams {
    message: Option<String>,               // 恒按 Markdown 渲染
    questions: Option<Vec<AskQuestion>>,   // 省略/空 → message 作为单问题
    files: Option<Vec<String>>,
    // 不暴露 markdown/single/selectOnly：markdown 恒 on，single/selectOnly 属脚本/纯文本场景
}
struct AskQuestion { question: String, options: Option<Vec<AskOption>> }
struct AskOption { text: String, #[serde(default)] recommended: bool }
```
每字段加 `#[schemars(description = "...")]`，供 `tools/list` 暴露给模型；`message` 描述需标注「按 Markdown 渲染」。

### 3.1.2 输出 Schema（`schemars` 派生，声明 outputSchema）

基于 `cli::output::render_json` 的形态（但**去掉 `selectedIndices`**——那是脚本专用，MCP 不需要），定义可序列化/可生成 schema 的结果类型，并在 `ask` 工具上声明为 **output schema**（rmcp 经 `#[tool]` 的输出类型 / 显式 outputSchema 暴露；确切 API 实现期对照 rmcp 文档）：

```rust
struct AskResult {
    action: String,
    channel: String,
    status: Option<String>,   // 仅 action=="cancel" 时出现：取消引导，要求重新确认
    answers: Vec<AskAnswer>,
}
struct AskAnswer {
    question_index: usize,
    selected_options: Vec<String>,
    user_input: Option<String>,
    files: Vec<String>,
    // 注意：不含 selected_indices（脚本专用）
}
```

> 实现：把子进程 `--output json` 的输出**反序列化进 `AskResult`**（serde 自动忽略多余的 `selectedIndices`），再以 `AskResult` 重新序列化为 `structuredContent`——既剔除了脚本字段，又保证输出与声明的 output schema 严格同构。`status` 由 CLI `--output json` 顶层产出（取消路径才有），薄壳原样透传。

### 3.2 Schema → argv

按现有 CLI 语义构造 argv 向量（无 shell，免转义）：
- `message`：作为首个位置参数（仅当无 `questions` 或与 CLI 一致地共享描述）；
- 每个 question → `-q <text>`；其 option → `-o <text>`（`recommended` 用 `-o!`）；
- 每个 file → `-f <path>`；
- 恒附 `--output json`（结构化结果，便于解析与结构化回传，见 3.4）。不传 `--no-markdown`/`--single`/`--select-only`（已不在 MCP 暴露）。

> 与 CLI 归一化对齐：无 `questions` 时把 `message` 当单问题；有 `questions` 且也有 `message` 时 `message` 为共享描述。具体拼装复用对 `cli/args.rs` 语义的理解，必要时抽 `argv` 构造小函数 + 单测。

### 3.3 spawn 子进程

- 取自身可执行路径 `std::env::current_exe()`（即 `AskHuman`），`tokio::process::Command` spawn `<exe> <argv...>`，继承 env 与 cwd（agent 探测/来源名/项目归类自然生效）。
- `await` 子进程结束，捕获 stdout、stderr、exit code。stderr 仅诊断（可丢弃或转 MCP server 日志）。
- 子进程长跑（人类思考可达小时级）属正常；MCP server `await` 即可，不设自有超时（超时由客户端 MCP 配置控制）。

### 3.4 结果 → CallToolResult（结构化 + 图片）

- 解析子进程 stdout（`--output json`）反序列化进 `AskResult`（忽略脚本专用的 `selectedIndices`）。
- **structuredContent** = `AskResult` 重新序列化（`{action, channel, answers:[{questionIndex, selectedOptions, userInput?, files[]}]}`，与声明的 output schema 同构、不含 `selectedIndices`）。
- **content** 数组：
  - 一段 `TextContent` = 序列化后的 JSON 字符串（MCP 规范：返回结构化结果时同时给文本兜底，便于不支持 structuredContent 的客户端）。
  - 每张图片一个 `ImageContent`：从 `answers[].files` 取**图片扩展名**（png/jpg/jpeg/gif/webp/...）路径，读字节 → base64 + mimeType → `Content::image(...)`；非图片路径不内联（已在 JSON `files` 中）。
  - 取图片为纯函数 `image_paths_from_result(&Value) -> Vec<PathBuf>` + 单测（单题/多题/无 files/图片与非图片混合/路径含空格）。
- **退出码映射**：exit 0/1 → 正常返回（1=取消/未作答，`action:"cancel"` 或空 `answers`）；exit 3 或 spawn 失败 → `CallToolResult` 标 `is_error=true` 并带错误文本（"failed to reach AskHuman daemon" 等）。
- 组装：`CallToolResult { structured_content: Some(json), content: [TextContent, ImageContent...], is_error }`（字段名以 rmcp 实际 API 为准）。

## 4. 双版本提示词（`src-tauri/src/prompts.rs`）

- 保留 `cli_reference()`。新增 `mcp_reference()`：基于同一交互纪律，但把工具用法从「Shell 调 `AskHuman` + 24h 超时 + 先跑 `--agent-help`」改为「调用 MCP 工具 **`ask`**」：
  - 删去「set that tool call's timeout to 24h」「run `--agent-help`」等 Shell 专属句；
  - 把 `the {program} command through the Shell/Bash tool` 改为 `the \`ask\` MCP tool`；
  - 保留：必须提问、不在直接输出/结束回合提问、提供预定义选项 + 标推荐、附件经工具、结束前回执、relentless interview、不擅自改方案等。
  - 选项推荐：MCP 入参用 `options[].recommended=true` 表达（替代 `-o!`）；附件用入参 `files`。
- 二者共享的不变片段可抽公共函数减少漂移（实现期视体量决定）。

## 5. MCP 配置集成（新 `src-tauri/src/integrations/mcp_config.rs` + `paths.rs`）

沿用现有 hook/rule 集成的**纯函数 + 最小化编辑 + 解析失败即中止 + 单测**范式。

### 5.1 新增 paths
- `cursor_mcp_json()` = `~/.cursor/mcp.json`
- `claude_json()` = `~/.claude.json`
- （Codex 复用现有 `codex_config_toml()`）

### 5.2 写入规范（server 名恒 `askhuman`，command = `current_exe()` 绝对路径，args = `["mcp"]`）
- **Codex（`toml_edit`）**：在 `config.toml` upsert `[mcp_servers.askhuman]`：`command`/`args`/`startup_timeout_sec=30`/`tool_timeout_sec=86400`。保留用户其它表与注释（toml_edit 已在 lifecycle codex 集成用过）。
- **Cursor（`jsonc-parser` CST）**：在 `mcp.json` 的 `mcpServers` 对象 upsert `askhuman`：`{ command, args }`。最小化编辑、保留注释/格式（同 `cursor_hook.rs` 手法）。**不写 `timeout`**（Cursor 超时 ~60s 硬编码不可配）。
- **Claude（`jsonc-parser` CST）**：在 `~/.claude.json` top-level `mcpServers` upsert `askhuman`（文件大、含大量项目历史 → 必须最小化编辑，绝不整写）：`{ command, args, timeout: 86400000 }`。**`timeout`(毫秒) 覆盖 Claude Code CLI 的 60s 默认**（MCP TS SDK `DEFAULT_REQUEST_TIMEOUT_MSEC`），否则长等待被 `-32001` 取消（`CLAUDE_TOOL_TIMEOUT_MS`，`needs_update` 一并校验）。⚠️ 已知用户级加载 bug（spec §7），实现期实测；必要时加项目级 `.mcp.json` 回退（留 TODO，不在首版强求）。

### 5.3 API（与 cursor_hook 对称）
```rust
pub fn is_installed(target) -> bool;      // 含 askhuman server 条目
pub fn needs_update(target) -> bool;      // 已装但 command 路径/超时模板 ≠ 最新
pub fn install(target) -> Result<String>;
pub fn update(target) -> Result<String>;
pub fn uninstall(target) -> Result<String>;
pub fn reveal(target); pub fn open(target);
pub fn display_path(target) -> String;
```
均以 `AgentTarget`（cursor/claude/codex）分派；纯变换函数（`apply_install`/`apply_uninstall`）独立可测。

## 6. 模式编排（新 `src-tauri/src/integrations/agent_mode.rs`）

把「Rule + (Hook | MCP 配置)」聚合为每家 agent 的**三态模式**，供 UI / CLI 复用。

```rust
pub enum Mode { None, Cli, Mcp }

pub fn current(target) -> Mode;     // 以 Rule 正文变体为准（见下），辅以 hook/config 存在性
pub fn needs_update(target) -> bool;// 当前模式各产物是否有更新
pub fn set(target, mode) -> Result<()>;  // 一键切换：先卸其余模式全部产物，再装目标模式
```

- **Rule 变体**：`agent_rules` 需支持两种正文。扩展为：
  - `install(target, variant)` / `update(target, variant)`，`variant ∈ {Cli, Mcp}` 决定写 `cli_reference()` 还是 `mcp_reference()`；
  - 新增 `installed_variant(target) -> Option<Variant>`：`block_body` == `mcp_reference()` → Mcp；== `cli_reference()` → Cli；否则（旧版/漂移）按 Cli 兜底且 `needs_update=true`。
  - `needs_update(target, variant)`：与对应 variant 正文比对。
  - 既有调用点（`agents_cmd`、`commands.rs`）改为传入 variant（默认 Cli 保持现状语义）。
- `current(target)`：`installed_variant` → Mode（Mcp/Cli）；无 Rule 且无 hook/config → None。
- `set(target, Mode::Cli)`：卸 MCP 配置 → 装 CLI Rule + 超时 Hook（codex 跳过 hook）。
- `set(target, Mode::Mcp)`：卸超时 Hook → 装 MCP Rule + MCP 配置。
- `set(target, Mode::None)`：卸当前模式全部产物（Rule + hook/config）。
- 切换天然幂等（各底层 install/uninstall 已幂等）。

> lifecycle hook（实验性 turn 追踪）**不进** `agent_mode`，保持现有独立开关，与模式正交（spec D9）。

## 7. 前端命令与设置 UI

### 7.1 后端命令（`src-tauri/src/commands.rs` + `src/lib/ipc.ts` + `types.ts`）
- 新增（入参 `agent`：cursor/claude/codex）：
  - `agent_mode_status(agent) -> { mode: "none"|"cli"|"mcp", needsUpdate, products: {...} }`
  - `agent_mode_set(agent, mode)`（一键切换，调用 `agent_mode::set`）
  - `agent_mode_update(agent)`（刷新当前模式产物到最新）
  - `mcp_config_reveal(agent)` / `mcp_config_open(agent)`（打开/定位配置文件）
  - `get_prompt(variant)`：扩展现有 `get_prompt` 以按 `cli`/`mcp` 返回提示词正文（供手动集成切换展示）。
- 现有 `cursor_hook_*`/`claude_hook_*`/`agent_rule_*` 命令保留（被 `agent_mode` 内部复用或前端旧路径过渡期共存；最终 UI 以 `agent_mode_*` 为主）。

### 7.2 设置「Agent」Tab（`src/views/SettingsView.vue` + i18n）
- **手动集成区**：参考提示词卡加 `CLI | MCP` 切换；MCP 视图下追加「MCP 配置实例」代码块（按当前 agent 给出 Codex toml / Cursor·Claude json 片段，含绝对路径占位说明）。
- **自动集成区**：每家 agent 一个卡片，顶部三态分段控件 `CLI | MCP | 未集成`（互斥、单选）：
  - 切换即调 `agent_mode_set`；选「未集成」即卸载。
  - 卡片下展示当前模式绑定产物及路径（CLI：Rule + 超时 Hook〔codex 注「无超时 hook」〕；MCP：Rule + MCP 配置），过期显示橙色「更新」（调 `agent_mode_update`）。
  - 产物「打开/在 Finder 显示」下拉复用现有交互。
- 文案全部进 `src/i18n/{en,zh}.ts`。

## 8. headless CLI 与体检（`cli/agents_cmd.rs` + `cli/doctor.rs`）

- `agents install/uninstall/update <agent>` 增 `--mcp`（与现 `--rules`/`--hook`/`--lifecycle` 并列），或新增 `agents mode <agent> <cli|mcp|none>` 子命令做一键切换（实现期二选一：倾向新增 `--mcp` 走 `agent_mode`，对齐「至少选一项」语义并支持互斥）。
  - 说明：`--mcp` 安装/更新/卸载 = MCP 模式产物（Rule(MCP) + MCP 配置）；与 `--hook` 互斥提示。
- `agents show`：在每家状态里增「mode」「MCP 配置」行（已装/需更新/路径）。
- `doctor`：集成体检段补每家「当前模式 + 各产物状态」。
- 帮助文案（`help()` / `cfgio::t`）同步双语更新。

## 9. 测试

- `prompts.rs`：`mcp_reference()` 含 `ask`、不含 Shell/24h/agent-help 片段（断言）。
- `mcp/ask.rs`：`image_paths_from_result`（解析 JSON 取图片路径）纯函数单测（单题/多题/无 files/图片+非图片混合/路径含空格）；Schema→argv 构造单测（message/questions/options/recommended/files/flags 组合）；从真实 `ToolRouter` 断言 `ask.inputSchema` 的 question / option item 已内联且无 `$ref`；`AskResult` 能反序列化 `render_json` 的输出（忽略 `selectedIndices`）并重新序列化出不含 `selectedIndices`、字段稳定的结构（防 output schema 漂移）。
- `integrations/mcp_config.rs`：三家 `apply_install`/`apply_uninstall` 幂等、保留用户内容/注释、解析失败中止、command 写绝对路径、Codex 超时字段；`needs_update`（路径漂移）。
- `integrations/agent_mode.rs`：`current` 变体识别（cli/mcp/none/旧版兜底）；`set` 切换后旧模式产物清除、新模式产物齐全（用临时 HOME 或纯函数层测）。
- `agent_rules.rs`：variant 写入/识别/needs_update 单测扩展。
- 端到端（install 后手动）：`AskHuman mcp` 在 Codex 注册后 `ask` 弹窗/IM 提问、长等不超时、图片直返；daemon 换新后 MCP server 续连。

## 10. 涉及文件清单

- 新增：`src-tauri/src/mcp/mod.rs`、`src-tauri/src/mcp/ask.rs`、`src-tauri/src/integrations/mcp_config.rs`、`src-tauri/src/integrations/agent_mode.rs`。
- 改：`src-tauri/Cargo.toml`（rmcp）、`src-tauri/src/main.rs`（`mod mcp;`）、`cli/mod.rs`（dispatch `"mcp"`）、`cli/help.rs`、`prompts.rs`（`mcp_reference`）、`paths.rs`（cursor_mcp_json/claude_json）、`integrations/mod.rs`（导出新模块）、`integrations/agent_rules.rs`（variant）、`cli/agents_cmd.rs`、`cli/doctor.rs`、`commands.rs`、`i18n.rs`。
- 前端：`src/lib/ipc.ts`、`src/lib/types.ts`、`src/views/SettingsView.vue`、`src/i18n/{en,zh}.ts`、（必要时）`styles/controls.css`。
- 文档：本计划 + `docs/specs/mcp.md`；完成后更新 `docs/overview.md` 与 `docs/wiki/`。

## 11. 任务顺序

1. 依赖 + `mcp` 子命令骨架：`Cargo.toml` 加 rmcp、`mcp/mod.rs` 跑通最小 `ask` 工具（回固定文本），`cli/mod.rs` 分支，`tools/list` 可见。
2. `ask` 真实逻辑：Schema→argv + spawn 子进程 + `extract_image_paths` + 组 TextContent/ImageContent（+单测）。
3. `prompts::mcp_reference()`（+单测）。
4. `integrations/mcp_config.rs` 三家配置写入（+纯函数单测）+ paths。
5. `agent_rules` 加 variant；`integrations/agent_mode.rs` 三态编排（+单测）。
6. `commands.rs` + `ipc.ts`/`types.ts` 新命令；`SettingsView.vue` 三态 UI + 手动集成 CLI/MCP 切换 + 配置实例 + i18n。
7. `agents_cmd.rs`/`doctor.rs` 纳入 MCP 模式 + 双语文案。
8. `cargo test`；按需 `./scripts/install.sh` 后在 Codex/Cursor/Claude 实测；更新 `docs/overview.md`。

## 12. 风险与注意

- **Claude `~/.claude.json`**：文件巨大且含会话历史，**必须最小化编辑**；用户级 `mcpServers` 有版本加载 bug，需实测，必要时回退项目级。
- **配置 command 路径**：写 `current_exe()` 绝对路径（客户端不继承 shell PATH）；多处安装/换新后路径可能变 → `needs_update` 以「路径 != 当前 exe」判定并提供「更新」。
- **结构化输出 API**：rmcp 声明 output schema + 返回 `structuredContent` 同时携带 `ImageContent` 的确切写法实现期对照 rmcp 文档；`AskResult` 必须与 `render_json` 形态严格同构（单测兜底）。
- **Codex 超时**：务必写大 `tool_timeout_sec`，否则默认 60s 仍会断；`startup_timeout_sec` 给足（server 启动很快，但留余量）。
- **Claude 超时**：Claude Code CLI 工具调用默认 60s，必须写 `mcpServers.askhuman.timeout`(毫秒) 覆盖；优先级 per-server `timeout` > `MCP_TOOL_TIMEOUT` 环境变量 > 60s 默认。（注意：Claude **Desktop** 忽略该字段、硬编码 60s，但我们面向的是 Claude Code **CLI** agent。）
- **互斥一致性**：`agent_mode::set` 必须先卸后装、幂等；异常中途失败要尽量保证不残留两套产物（实现期注意顺序与错误处理）。
- **图片体积**：base64 内联大图会涨 token/上下文（Codex 已按视觉 tile 估算缓解）；保持与 CLI 一致即可，不额外压缩。
- **平台**：`mcp` 子命令全平台可用；Windows 子进程走单进程弹窗（无 daemon、无排空概念，直接返回）。
