# 配置结构概览

> 本文是 `docs/overview.md` 的专题补充，记录配置模型与读取边界。用户向说明见 `docs/wiki/`，字段真值以 `src-tauri/src/config.rs` 为准。

## 文件与兼容性

- 主配置文件为 `~/.askhuman/config.json`；新位置缺失时回退旧 `~/.humaninloop/config.json`。
- 配置缺字段时取默认值，未知字段忽略，文件缺失或损坏时回退完整默认配置。
- Unix 下配置目录收紧为 `0700`，配置文件收紧为 `0600`。

## 字段地图

- `general`
  - `theme`、`language`、`alwaysOnTop`、`appearAnimation`、`windowEffect`（`solid|blur|glass`；配置默认 `blur`，显式 `glass` 在 macOS 26 以下有效值自动解析为 `blur`）
  - `speechLanguage`、`speechShortcut`
  - `historyLimit`（默认 200）与 `popupSound`
  - `menuBarIcon`（`off|active|always`，默认 `always`，macOS/Linux/Windows 桌面；已有显式值保持不变）
  - `popupPrewarm`（默认 `true`，macOS/Linux/Windows 桌面）
  - `daemonLifecycle`（`activity|keepalive`，默认 `activity`，macOS/Linux/Windows）
- `channels.popup`：`enabled`、`width`、`height`、`rememberSize`
- `channels.telegram`：`enabled`、`botToken`、`chatId`、`apiBaseUrl`
- `channels.dingding`：`enabled`、`clientId`、`clientSecret`、`userId`、普通提问/确认/权限卡片模板 ID，以及文本附件内联/转 docx 开关；Agent 任务输入复用普通提问模板
- `channels.feishu`：`enabled`、`appId`、`appSecret`、`openId`、`baseUrl`
- `channels.slack`：`enabled`、`botToken`、`appToken`、`userId`
- `channels.autoActivation`：IM 渠道按需发送，默认 `false`
- `channels.autoEndWatch`：切离活跃 IM 后自动结束该渠道 watch，默认 `true`，仅在 `autoActivation` 开启时生效
- `agentTasks.enabled`：从 IM 创建电脑端 Agent 任务，默认 `false`；开启会强制 `daemonLifecycle=keepalive` 并同步 daemon 登录项。入口在设置「高级」Tab（2026-07-25 从「实验」转正）
- `agentTasks.permissionPrompt`：`ask|agent-default|yolo`，默认 `ask`
- `experimental.enabled`：显示实验区，默认 `false`（当前实验区为空，留作下一个实验功能的落点）
- `experimental.verticalQuestions`：多问题纵向显示，默认 `false`

## 读取入口

- `AppConfig::load()` 解析完整配置和系统钥匙串中的密钥；Daemon、IM 渠道、设置页密钥状态等确需凭据的路径使用它。
- `AppConfig::load_without_secrets()` 只读非密钥配置；语言、主题、历史上限、纯信息命令及窗口创建等路径使用它，避免无关操作触发钥匙串访问。
- `AppConfig::save()` 负责原子写入，并将受管密钥迁移或写入系统钥匙串；只持有非密钥配置的调用不会清空既有密钥。

密钥存储和 macOS 会话边界见 `docs/specs/secret-storage-keychain.md`。

Agent 任务的 workspace 索引独立存于 `~/.askhuman/agent-workspaces.json`，一次性启动记录存于
`~/.askhuman/state/agent-launches/`；二者都不是 `AppConfig`。

轻量界面状态（如弹窗「配置 IM 渠道」一次性引导的已关闭标记）独立存于
`~/.askhuman/ui-state.json`（`src-tauri/src/uistate.rs`）：读失败视为默认、写失败静默，
与用户配置分离以免互相触发保存/迁移逻辑。
各 Agent 的 capability 偏好也不在 `AppConfig` 里，各自独立成文件：权限审批
`~/.askhuman/permission-preferences.json`、结束确认 `~/.askhuman/stop-preferences.json`、
接管 Claude 内置提问 `~/.askhuman/ask-question-preferences.json`（默认开，见
`docs/specs/claude-ask-user-question.md`）。它们与 `agents mode` 正交：偏好长期保存，
只有 CLI/MCP mode 才把对应 Hook 落到磁盘。
