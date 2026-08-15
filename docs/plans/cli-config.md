# CLI 配置与 Agent 集成 — 实现计划

> Windows 注（2026-08）：本文保留首期无 daemon 降级步骤；当前 Windows CLI 已连接 shared daemon，
> channel/detect/doctor/monitor 与 Agent 集成不再走 unsupported 分支。

> 需求见 `docs/specs/cli-config.md`。本计划只描述方案与必要规则，不堆具体代码。

## 0. 总体思路

- 复用既有读写与业务核心，CLI 只做「解析 argv → 调用核心 → 本地化输出」：
  - 配置读写：`config.rs` 的 `AppConfig::load() / load_without_secrets() / save()`（save 已自动把密钥写钥匙串、文件 0600）。
  - 密钥账户：`config.rs::SECRET_SPECS` + `secrets::{get,set,delete}`。
  - 集成：`integrations::{agent_rules, cursor_hook, claude_hook, agent_lifecycle}` 现有 `install/update/uninstall/status/is_installed/needs_update/supported`。
  - 测试 / 识别：把 `commands.rs` 里 `*_test` / `*_detect_*` 的**核心逻辑抽成普通 async 函数**（与 `#[tauri::command]` 包装解耦），CLI 与 GUI 共用。`detect` 仍经 daemon `ipc::DetectRequest` 复用单连接。
  - Agent 实时状态：复用 daemon `AgentsSubscribe` / `AgentsState`（`monitor --json/文本` 取一次快照即可，无需开窗）。
- 配置落盘后，daemon 的 `config_watch` 会自动热重载。**仅改密钥（钥匙串）而 `config.json` 未变**的情况：写完后对 `config.json` 做一次原子重写（内容可不变）以触发 watcher 重连（见 §6）。

## 1. 目录与分发

- `cli/mod.rs::dispatch` 新增 match 臂：`"channel"` / `"config"` / `"doctor"`；扩展现有 `"agents"`。
- 新增模块（每个含子分发 + `help` 文案 + JSON/文本渲染）：
  - `cli/channel_cmd.rs`、`cli/agents_cmd.rs`（并入现有 `agents_dispatch`）、`cli/config_cmd.rs`、`cli/doctor.rs`。
  - 公共小工具放 `cli/cfgio.rs`：点号路径 get/set（基于 `serde_json::Value`）、密钥键识别、密钥取值（env/file/stdin/交互隐藏）、bool/枚举解析、表格/JSON 输出助手。
- 这些命令多为 async（test/detect/monitor 要跑 tokio + 连 daemon）：用与现有 client 路径一致的运行时（`tauri::async_runtime::block_on` 或局部 `tokio::runtime`）。纯文件类（config/channel set 非密钥）可同步。

## 2. `channel` 组

### 2.1 字段 / flag 映射（非交互）
| 渠道 | 开关 | 非密钥字段 flag | 密钥字段（仅 env/file/stdin） |
|---|---|---|---|
| telegram | `--enable`/`--disable` | `--chat-id` `--api-base-url` | `--bot-token-{env\|file\|stdin}` |
| dingding | 同上 | `--client-id` `--user-id` `--card-template-id` `--inline-small-text <bool>` `--convert-text-to-docx <bool>` | `--client-secret-{env\|file\|stdin}` |
| feishu | 同上 | `--app-id` `--open-id` `--base-url` | `--app-secret-{env\|file\|stdin}` |
| slack | 同上 | `--user-id` | `--bot-token-{…}` `--app-token-{…}` |

- 解析后：`AppConfig::load()` → 改对应字段 → `save()`（密钥经 `save()` 入钥匙串）。`--enable/--disable` 改 `enabled`。
- 字段名为 kebab，对应 config 的 camelCase；与 `config set channels.<name>.<field>` 等价（`channel` 只是更友好的封装 + 校验 + 引导）。

### 2.2 交互向导（终端且无 flag）
- 逐项 prompt（显示当前值、回车保留）：开关 → 各非密钥字段 → 各密钥（隐藏输入，留空保留）。
- 末尾可选：`detect`（识别 userId/openId 回填）→ `test`（发测试消息）→ 保存。
- 非 TTY（管道）但无 flag：报错并提示「用 flag 或在终端运行」，避免脚本阻塞挂起（参考 `--stdin` 的 TTY 检测）。

### 2.3 `test` / `detect`
- `test <name>`：调 §0 抽出的核心（telegram/feishu/dingding 走 HTTP 发送，不占长连；slack 另探 socket url）。读未提供的密钥用 `fallback_secret`（即已存钥匙串的值）。
- `detect <name>`（dingding/feishu/slack；telegram 用 chatId 不需要）：经 daemon `DetectRequest` 等用户给 bot 发「识别码」消息，回填 userId/openId；可追问是否写入配置。

## 3. `agents` 组（状态 + 集成）

- **改名**：现 `agents_dispatch` 的 `"status"` 臂改为 `"monitor"`；`agents status` 不再保留（实验功能，无需别名）。
- `monitor [--json]`：有 GUI 平台默认开窗（现 `app::run_agents`）；`--json` 或无 GUI → 连 daemon 取一次 `AgentsState` 快照，按 working/idle/ended 渲染文本或 JSON（复用 `autochannel::status_text` 风格或直接序列化快照）。
- 集成动词 → 现有函数（按 flag 选类，agent 名定 target）：
  | flag | cursor | claude | codex |
  |---|---|---|---|
  | `--rules` | `agent_rules::*` (AgentTarget) | 同 | 同 |
  | `--hook` | `cursor_hook::*` | `claude_hook::*` | 不支持（提示跳过） |
  | `--lifecycle` | `agent_lifecycle::*` (AgentKind) | 同 | 同 |
  - `install`：无任何 flag → 报错列出可选项（D6）。`uninstall`/`update` 同按 flag 选类。
  - `--lifecycle` 为实验项：可正常安装，但在 `show`/输出里标注「实验性」。
- `show [<agent>]`：打印 `prompts::cli_reference()`（手动集成提示词）+ 每 agent 粘贴位置（`agent_rules::display_path`）+ 三类 `is_installed`/`needs_update` 状态。无 agent 参数 → 三家都列。

## 4. `config` 组（兜底）

- `show [--json]`：`load()` → 序列化为 `Value`；密钥字段（按 §0 secret 路径集）渲染为 `●●●`（已设）/ 空（未设），其余原样。
- `get <key>` / `set <key> <value>` / `unset <key>`：在 `Value` 上按点号路径定位（camelCase）。
  - `set`：非密钥键直接写值（按目标类型解析 bool/数字/字符串/枚举）→ 反序列化回 `AppConfig` → `save()`；未知键或类型不符报错。
  - **密钥键**（`channels.dingding.clientSecret` / `channels.feishu.appSecret` / `channels.telegram.botToken` / `channels.slack.botToken` / `channels.slack.appToken`）：忽略 argv 值，改从 `--from-env <VAR>` / `--from-file <path>` / `--from-stdin` 取，写钥匙串（D5）。
  - `unset`：把该键设回该字段的默认值（用 `Default` 子结构取默认）；密钥键 → `secrets::delete`。
- `path`：打印 `paths::config_file()`。

## 5. `doctor`

- 汇总渲染（文本 / `--json`）：
  - daemon：是否在跑（复用 `client` 的 status 探测）、版本、在途数、当前 IM 连接。
  - 渠道：逐个 启用 / 配置齐全（必填字段 + 密钥非空）/（daemon 在跑时）已连接。
  - 集成：逐 agent × 三类 装没装 / 需更新（`is_installed`/`needs_update`）。
- 退出码：全部健康 0；有「未配置但已启用」「需更新」等可作非零或仅文本提示（取一致策略，默认 0 + 文本，`--json` 机读）。

## 6. 落盘与热重载

- 一般字段改 `config.json` → watcher 自动重载。
- 仅密钥变更（写钥匙串、`config.json` 内容未变）：保存后对 `config.json` 再做一次 `save()`（原子重写，mtime 变）以触发 watcher 重连，确保 daemon 即时用上新密钥。

## 7. i18n 与 help

- 新增 i18n key 前缀（如 `cli.channel.*` / `cli.agents.*` / `cli.config.*` / `cli.doctor.*`）：向导提示、字段说明、状态标签、错误。
- help：`AskHuman channel|agents|config help` 给组级用法；各子命令 `--help` / `help <sub>` 给字段级用法与脚本示例（D9）。`cli/help.rs` 现有结构扩展。

## 8. 跨平台 / 降级

- 文件类（config / channel set 非密钥 / agents 集成安装）全平台可用。
- 依赖 daemon 的（test 的 slack socket 探测除外的连接、detect、monitor 窗口与连接状态）在无 daemon 平台（Windows）给「该能力需 daemon（当前平台暂不支持）」提示。

## 9. 验证

- 单测：点号路径 get/set/unset（含密钥路由）、字段 flag 解析、bool/枚举解析、密钥取值（env/file/stdin）纯函数。
- 端到端（`./scripts/install.sh` 后）：`channel set` 脚本式与向导式各跑一遍 + `test`/`detect`；`agents install/uninstall/update/show` 三家；`config show/get/set/unset`；`doctor`；`agents monitor --json`。

## 10. 实施顺序

1. 抽 `commands.rs` 的 test/detect 核心为共享函数（不改 GUI 行为）。
2. `cli/cfgio.rs`（路径 kv + 密钥取值 + 输出助手）+ 单测。
3. `config` 组 → `channel` 组（含向导/脚本/ test/detect）。
4. `agents` 组：先 `status`→`monitor` 改名 + 文本/json，再集成动词与 `show`。
5. `doctor`。
6. i18n / help / 文档（overview、PROGRESS）。
