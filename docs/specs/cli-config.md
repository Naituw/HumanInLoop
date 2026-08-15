# CLI 配置与 Agent 集成（无 GUI / headless 场景）— 需求

> 状态：已实现；后续新增 `agents stop`，其余命令面保持本文设计。

## 背景与动机

当前「渠道配置」「Agent 集成（Rules / 超时 Hook / 生命周期 Hook）安装」都只能通过 GUI
（设置窗口 `--settings`）完成。Linux 服务器 / 容器 / SSH 等 **无 GUI** 环境下用户无法配置，
只能手改 `~/.askhuman/config.json` 并自行操作 `~/.cursor` `~/.claude` `~/.codex`。

本需求在 CLI 中补齐这些能力，使**纯命令行**即可完成全部配置与集成，且**可脚本化一次性执行**
（用户可能预写脚本批量执行，而非在终端前逐项输入）。

## 范围

1. **渠道配置**：telegram / dingding / feishu / slack 的启用、字段、密钥、连通性测试、userId/openId 自动识别。
2. **Agent 集成**：cursor / claude / codex 的手动集成（打印参考提示词）+ 自动集成（安装 Rules / 超时 Hook / 生命周期 Hook）。
3. **通用配置兜底**：对 `config.json` 任意键的读写（含非渠道项：主题、语言、历史上限、自动激活、实验开关等）。
4. **体检**：一屏汇总 daemon / 渠道 / 集成 的健康状态，便于 headless 排障。
5. **headless 友好**：原本仅 GUI 的 Agent 实时状态窗口，提供文本 / JSON 输出。

Windows named-pipe daemon 已在平台对齐项目落地；本命令面在 macOS、Linux 与 Windows 共用同一 daemon 状态和集成模块。

## 命令总览（锁定）

新增 / 调整四个顶层子命令组，沿用现有 `daemon` 风格；每个组与子命令都提供 `help`。

### `AskHuman channel` —— IM 渠道配置（主入口，强引导 + 可脚本）
- `channel list [--json]` —— 列出各渠道：启用? / 配置齐全? /（daemon 在跑时）已连接?
- `channel set <name> [flags]` —— **二合一**：
  - **终端且不带 flag → 交互向导**：逐项提示、密钥隐藏输入、可顺带 detect / test。
  - **带 flag → 非交互（脚本用）**：如
    `AskHuman channel set telegram --enable --chat-id 123 --bot-token-env TG_TOKEN`
- `channel enable|disable <name>`
- `channel test <name>` —— 经各渠道 client 发一条测试消息（不占用 daemon 长连接）。
- `channel detect <name>` —— 交互式自动识别 userId / openId（提示给 bot 发消息 → 捕获 → 可保存）。
- name ∈ telegram | dingding | feishu | slack。

### `AskHuman agents` —— Agent 状态 + 集成（合并；解决与原 `agents status` 命名冲突）
- `agents monitor [--json]` —— **原 `agents status` 改名为此**：实时状态。
  有 GUI → 开状态窗口；headless / `--json` → 文本 / JSON 输出。
- `agents mode <agent> [none|cli|mcp]` —— 查询或设置自动集成 mode；设置时完整 reconcile 目标整包，重复
  设置同值也等同显式更新。
- `agents update [<agent>]` —— 更新单家或全部当前非 None / 有托管残留的整包。
- `agents permission <claude|codex> [on|off]` —— 查询或设置独立 PermissionRequest 审批 capability。
- `agents lifecycle <agent> [on|off]` —— 查询或设置独立生命周期追踪 capability。
- `agents stop <claude|codex|cursor> [on|off]` —— 查询或设置自然 Stop 时的结束确认；Grok 不支持。
- `agents show [<agent>]` —— **手动集成**：打印参考提示词 + 粘贴位置 + mode/rules/timeout/
  permission/MCP/lifecycle/stop 状态。
- agent ∈ cursor | claude | codex | grok（grok 仅 none|mcp）。旧 `install/uninstall` 和逐产物写 flags
  已移除：不得部分执行，非零退出并给 mode/update/permission/lifecycle 迁移提示。

### `AskHuman config` —— 通用键值（兜底）
- `config show [--json]` —— 打印生效配置（密钥脱敏为 `●●●`，标注已设 / 未设）。
- `config get <key>`
- `config set <key> <value>` —— 点号小驼峰键，如 `general.language`、`channels.autoActivation`、
  `channels.telegram.chatId`。**密钥键自动路由进钥匙串**，其值仍只从 stdin / env 取（不进 argv）。
- `config unset <key>` —— 重置为默认。
- `config path` —— 打印 `config.json` 路径。

### `AskHuman doctor [--json]` —— 一屏体检
daemon 是否在跑 / 各渠道（启用·配置齐全·连接）/ 各 agent 集成（Rules·Hook·生命周期 装没装·是否需更新）。

## 关键决策（访谈锁定）

- **D1 命名空间**：三组 `channel` / `agents` / `config` + 顶层 `doctor`（贴合现有 `daemon`/`agents` 风格）。
- **D2 渠道配置主入口**：`channel`（强引导、可交互多步）；`config` 仅作通用兜底。
- **D3 `channel set` 形态**：二合一——终端且无 flag 走交互向导；带 flag 走非交互（脚本）。
- **D4 密钥输入**：脚本化用 `--<field>-env <VAR>` / `--<field>-file <path>` / `--<field>-stdin`（或值 `-`）；
  交互时隐藏输入。**不**接受密钥明文直接进 argv（避免泄漏 shell 历史 / `ps`）。
- **D5 `config` 可设密钥键**：自动路由进钥匙串，值仍从 stdin / env 取。
- **D6 集成写入统一为 mode 整包**：不允许用逐产物 flags 拼装半安装状态；permission 与 lifecycle 保持
  独立 on/off。重复设置当前 mode 仍完整更新磁盘，但绝不改写 permission preference。
- **D7 纳入**：`channel detect`、`doctor`、所有列表 / 状态 / 体检的 `--json`。
- **D8 改名**：原 `agents status`（GUI 状态窗口）→ `agents monitor`，并增加文本 / `--json`。
- **D9 每个子命令都要有 `help`** 引导配置。
- **D10 本地化**：所有面向用户输出复用现有 i18n（中 / 英）。
- **D11 跨平台**：macOS、Linux 与 Windows 全部可用；`test`、`detect`、`monitor` 与连接状态均查询 shared daemon，Windows 使用 named pipe，不维护单独降级语义。

## 反馈意见

（后续讨论 / 调整记录追加于此）
