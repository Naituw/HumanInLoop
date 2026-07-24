//! 应用配置：`~/.askhuman/config.json` 读写、默认值、容错解码。
//! 读取时若新位置缺失则回退旧 `~/.humaninloop/config.json`（向后兼容）。

use crate::paths;
use crate::secrets;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Maps each channel secret to its keychain account and its `&mut String` field in `AppConfig`.
/// Drives resolve/migrate/persist uniformly so the policy is written once for all three secrets.
struct SecretSpec {
    account: &'static str,
    field: fn(&mut AppConfig) -> &mut String,
}

const SECRET_SPECS: [SecretSpec; 5] = [
    SecretSpec {
        account: secrets::ACCOUNT_DINGTALK_SECRET,
        field: |c| &mut c.channels.dingding.client_secret,
    },
    SecretSpec {
        account: secrets::ACCOUNT_FEISHU_SECRET,
        field: |c| &mut c.channels.feishu.app_secret,
    },
    SecretSpec {
        account: secrets::ACCOUNT_TELEGRAM_TOKEN,
        field: |c| &mut c.channels.telegram.bot_token,
    },
    SecretSpec {
        account: secrets::ACCOUNT_SLACK_BOT_TOKEN,
        field: |c| &mut c.channels.slack.bot_token,
    },
    SecretSpec {
        account: secrets::ACCOUNT_SLACK_APP_TOKEN,
        field: |c| &mut c.channels.slack.app_token,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    System,
    Light,
    Dark,
}

/// 弹窗出现动画样式（对应 macOS `NSWindowAnimationBehavior`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PopupAnimation {
    /// 无动画（NSWindowAnimationBehaviorNone = 2）。
    None,
    /// 文档窗口动画（NSWindowAnimationBehaviorDocumentWindow = 3）。
    Document,
    /// 提示面板动画（NSWindowAnimationBehaviorAlertPanel = 5），更明显。
    #[default]
    Alert,
}

impl PopupAnimation {
    /// 映射到 macOS `NSWindowAnimationBehavior` 原始取值。
    #[cfg(target_os = "macos")]
    pub fn ns_animation_behavior(self) -> isize {
        match self {
            PopupAnimation::None => 2,
            PopupAnimation::Document => 3,
            PopupAnimation::Alert => 5,
        }
    }
}

/// Menu bar / tray icon mode (spec D4). Available on macOS and Linux desktops; hidden on Windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MenuBarIconMode {
    /// Hide the icon while keeping the GUI host available on demand for singleton windows.
    Off,
    /// Show the icon while the daemon is active and exit the host after the daemon becomes idle.
    Active,
    /// Keep the icon resident and launch it at login; show the stopped state when the daemon exits.
    #[default]
    Always,
}

/// 守护进程生命周期模式（实验 Tab，仅 Unix daemon 有意义；Windows 无 daemon、忽略）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DaemonLifecycleMode {
    /// 按 agent 活动：首次提问 / hook 拉起，无在途/无「工作中」agent 且空闲超时后自动退出。默认（旧行为）。
    #[default]
    Activity,
    /// 保活：不再空闲退出；装 daemon 登录项开机自启（作用于 daemon 本体，类似托盘 always）。
    /// 让 IM 随时可收消息，代价是常驻少量资源 + 保持 IM 通道连接。
    KeepAlive,
}

/// macOS 窗口材质。Glass 在不支持 Liquid Glass 的系统上解析为 Blur。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WindowEffect {
    /// macOS 26+ Liquid Glass（`NSGlassEffectView`，由插件应用）。
    Glass,
    /// 传统毛玻璃模糊（`NSVisualEffectView` / UnderWindowBackground）。
    /// 默认值：Glass 下文字可读性受壁纸影响大，macOS 26+ 也默认用 Blur。
    #[default]
    Blur,
    /// 完全不透明的主题纯色，不使用任何 Visual Effects 视图。
    Solid,
}

impl WindowEffect {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Glass => "glass",
            Self::Blur => "blur",
            Self::Solid => "solid",
        }
    }
}

/// Global collaboration style for installed agent prompts (spec collaboration-style.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum CollaborationStyle {
    /// Relentless interview until shared understanding (historical default).
    #[default]
    Aligned,
    /// Fewer mid-task questions; ask on blockers / high blast radius only.
    Autonomous,
    /// User-supplied collaboration paragraph (`collaboration_style_custom_text`).
    Custom,
}

impl CollaborationStyle {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aligned => "aligned",
            Self::Autonomous => "autonomous",
            Self::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "aligned" => Some(Self::Aligned),
            "autonomous" => Some(Self::Autonomous),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// Popup / Confirm submit keyboard shortcut mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum PopupSubmitKey {
    /// ⌘/Ctrl+Enter submits; plain Enter and other modified Enter insert newline. Default.
    #[default]
    CmdEnter,
    /// Bare Enter submits (same multi-question advance semantics); any modifier+Enter inserts newline.
    Enter,
}

impl PopupSubmitKey {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CmdEnter => "cmdEnter",
            Self::Enter => "enter",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GeneralConfig {
    pub theme: ThemeMode,
    /// 界面语言：`"auto"`（跟随系统）/ `"en"` / `"zh"`。回退英文。
    pub language: String,
    pub always_on_top: bool,
    pub appear_animation: PopupAnimation,
    pub window_effect: WindowEffect,
    /// 语音识别语言（BCP-47，如 "zh-CN"/"en-US"）；"auto" 表示跟随系统首选语言。
    pub speech_language: String,
    /// 语音输入快捷键（弹窗内）。规范串如 "cmd+d"/"cmd+shift+d"；空串表示关闭。
    pub speech_shortcut: String,
    /// 弹窗/Confirm 提交快捷键：`cmdEnter`（默认）或 `enter`。
    pub popup_submit_key: PopupSubmitKey,
    /// 协作风格：对齐 / 自主 / 自定义（写入 Agent rules/skill 的可替换段）。
    pub collaboration_style: CollaborationStyle,
    /// 自定义协作风格正文（英文契约段）；`collaboration_style == custom` 时使用，空则回退对齐默认。
    #[serde(default)]
    pub collaboration_style_custom_text: String,
    /// 回复历史保留条数上限。默认 200；`0` 表示停止新增记录（但保留并仍可查看旧记录）。
    pub history_limit: u32,
    /// 待办执行历史保留条数（按项目各留 N 条，第 16 轮定案）。默认 20；`0` 同 history_limit
    /// 语义：停止新增记录，既有历史保留。
    pub todo_history_limit: u32,
    /// Built-in sound played when a popup appears. Empty string disables it.
    /// macOS stores a sound name, such as "Glass"; Linux treats any non-empty
    /// value as enabled and plays a freedesktop notification sound.
    pub popup_sound: String,
    /// Menu bar / tray icon mode (off/active/always, spec D4). Defaults to always.
    pub menu_bar_icon: MenuBarIconMode,
    /// 弹窗预热（方案6）：daemon 常驻一个已挂载、隐藏待命的 `--popup --warm` 进程，来请求时直接喂
    /// `Show` 上屏（省掉 WebView 初始化 + 页面加载 + 挂载的关键路径开销）。默认开；可关（非实验项）。
    /// 代价是常驻一个隐藏 WebView 进程（少量内存）。无显示环境（headless）自动不生效。
    pub popup_prewarm: bool,
    /// 守护进程生命周期模式（activity 默认 / keepalive 保活）。UI 入口在「实验」Tab；仅 Unix daemon 有意义。
    pub daemon_lifecycle: DaemonLifecycleMode,
}

/// 回复历史默认保留条数。
fn default_history_limit() -> u32 {
    200
}

/// 待办执行历史默认保留条数（每项目）。
fn default_todo_history_limit() -> u32 {
    20
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            theme: ThemeMode::System,
            language: "auto".to_string(),
            always_on_top: true,
            appear_animation: PopupAnimation::Alert,
            window_effect: WindowEffect::Blur,
            speech_language: "auto".to_string(),
            speech_shortcut: "cmd+d".to_string(),
            popup_submit_key: PopupSubmitKey::CmdEnter,
            collaboration_style: CollaborationStyle::Aligned,
            collaboration_style_custom_text: String::new(),
            history_limit: default_history_limit(),
            todo_history_limit: default_todo_history_limit(),
            popup_sound: String::new(),
            menu_bar_icon: MenuBarIconMode::Always,
            popup_prewarm: true,
            daemon_lifecycle: DaemonLifecycleMode::Activity,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PopupChannelConfig {
    pub enabled: bool,
    pub width: f64,
    pub height: f64,
    pub remember_size: bool,
}

impl Default for PopupChannelConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            width: 560.0,
            height: 620.0,
            remember_size: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TelegramChannelConfig {
    pub enabled: bool,
    pub bot_token: String,
    pub chat_id: String,
    pub api_base_url: String,
}

impl Default for TelegramChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bot_token: String::new(),
            chat_id: String::new(),
            api_base_url: "https://api.telegram.org".to_string(),
        }
    }
}

/// 钉钉渠道配置。robotCode 不单独配置——企业内部应用机器人 robotCode = clientId(AppKey)。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DingTalkChannelConfig {
    pub enabled: bool,
    /// 企业内部应用 AppKey（同时用作机器人 robotCode）。
    pub client_id: String,
    /// 企业内部应用 AppSecret。
    pub client_secret: String,
    /// 接收/作答用户的 userId（单聊）。
    pub user_id: String,
    /// 互动卡片高级版模板 ID（可空）。留空则用代码内置默认模板（见 channels::dingding）。
    pub card_template_id: String,
    /// 通用确认卡模板 ID（`/stage` 等；双按钮 + finalized。见 docs/assets/dingtalk-confirm-card-template.json）。
    #[serde(default)]
    pub confirm_card_template_id: String,
    /// Permission approval single-select + Submit card template override.
    #[serde(default)]
    pub permission_confirm_card_template_id: String,
    /// 文本类附件：短文本（≤阈值）是否内联进消息正文（默认开）。见
    /// `docs/plans/dingtalk-attachment-preview.md`。
    pub inline_small_text: bool,
    /// 文本类附件：未内联的文本文件是否转 docx 发送（默认开）。关则发送源文件。
    pub convert_text_to_docx: bool,
}

impl Default for DingTalkChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            client_id: String::new(),
            client_secret: String::new(),
            user_id: String::new(),
            card_template_id: String::new(),
            confirm_card_template_id: String::new(),
            permission_confirm_card_template_id: String::new(),
            // 文本附件预览能力默认开启（旧配置缺字段时经 serde(default) 取此默认）。
            inline_small_text: true,
            convert_text_to_docx: true,
        }
    }
}

/// 飞书（Feishu / Lark）渠道配置。
/// 形态：企业自建应用 + 机器人 + 长连接(WebSocket) + 单聊；发消息统一用 tenant_access_token。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FeishuChannelConfig {
    pub enabled: bool,
    /// 企业自建应用 App ID（`cli_...`）。
    pub app_id: String,
    /// 企业自建应用 App Secret。
    pub app_secret: String,
    /// 接收/作答用户的 Open ID（单聊，发消息用 receive_id_type=open_id）。
    pub open_id: String,
    /// 开放平台域名：默认飞书国内 `https://open.feishu.cn`；Lark 国际版填 `https://open.larksuite.com`。
    pub base_url: String,
}

impl Default for FeishuChannelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            app_id: String::new(),
            app_secret: String::new(),
            open_id: String::new(),
            base_url: "https://open.feishu.cn".to_string(),
        }
    }
}

/// Slack 渠道配置。
/// 形态：Slack App + Socket Mode 长连接(WebSocket) + 机器人 + 单聊(DM)。
/// 鉴权双 token：Bot Token（`xoxb-…`，Web API 发送）+ App-Level Token（`xapp-…`，Socket Mode 建连）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
#[derive(Default)]
pub struct SlackChannelConfig {
    pub enabled: bool,
    /// Bot Token（`xoxb-…`）：所有 Web API 调用（chat.* / conversations.* / files.* / auth.test）。
    pub bot_token: String,
    /// App-Level Token（`xapp-…`，scope=connections:write）：Socket Mode 建连。
    pub app_token: String,
    /// 接收/作答用户的 Slack User ID（`U…`，单聊；发送前经 conversations.open 解析 DM 频道）。
    pub user_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ChannelsConfig {
    pub popup: PopupChannelConfig,
    pub telegram: TelegramChannelConfig,
    pub dingding: DingTalkChannelConfig,
    pub feishu: FeishuChannelConfig,
    pub slack: SlackChannelConfig,
    /// 「IM 渠道按需发送」开关（默认关 = 旧「每次提问全发所有启用 IM」行为；显式配置的用户设置保持原值）。
    /// 开启后：仅当前活跃槽对应的 IM 收提问卡片；在 agent 工作期间于某 IM 发 `/here`（或「这里」）
    /// 即把该渠道设为活跃槽。配置字段独立于 `experimental`，显式写入的用户选择跨版本保留。
    pub auto_activation: bool,
    /// 「自动结束 watch」——「按需发送」的子开关（**默认开**，仅 `auto_activation` 开时生效）。
    /// 开启后：当某真实 IM 渠道**不再是活跃槽**（活跃槽切到本地弹窗或别的 IM）时，自动结束该渠道上的
    /// 全部 watch（卡片就地定格「已切换到 XX · 自动结束关注」），省去回到电脑后手动 `/unwatch`。
    /// 见 `docs/specs/im-auto-end-watch.md`。
    pub auto_end_watch: bool,
}

impl Default for ChannelsConfig {
    fn default() -> Self {
        Self {
            popup: PopupChannelConfig::default(),
            telegram: TelegramChannelConfig::default(),
            dingding: DingTalkChannelConfig::default(),
            feishu: FeishuChannelConfig::default(),
            slack: SlackChannelConfig::default(),
            auto_activation: false,
            // 子开关默认开：老配置缺该字段时（容器级 `#[serde(default)]` 回退到此）按开处理。
            auto_end_watch: true,
        }
    }
}

/// 实验性高级功能（spec D15）：默认隐藏，需在「通用」Tab 底部的隐蔽开关里打开后才显示「实验」Tab。
/// 仅 macOS/Linux 暴露该设置；Windows 不显示（无 daemon / 生命周期追踪）。
/// 各 Agent 的「追踪开启」真值以 lifecycle hook 是否已安装为准（实时查询），故此处只需保存「是否显露实验区」。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExperimentalConfig {
    /// 是否显露「实验」Tab（隐蔽开关，默认关）。
    pub enabled: bool,
    /// 多问题弹窗是否「纵向同时显示所有问题」（默认关 = 旧版「一次一题 + 上/下一步」）。
    pub vertical_questions: bool,
}

/// IM-created Agent task settings. The feature is opt-in because enabling it also requires the
/// daemon to remain available while no local Agent is running.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentTasksConfig {
    pub enabled: bool,
    pub permission_prompt: AgentTaskPermission,
}

impl Default for AgentTasksConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            permission_prompt: AgentTaskPermission::Ask,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum AgentTaskPermission {
    #[default]
    Ask,
    AgentDefault,
    Yolo,
}

/// 权限确认相关全局设置（spec codex-permission-remember）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PermissionsConfig {
    /// Codex shell 宽松模式全局开关（D52）：非危险且可解析的 shell 命令自动放行，
    /// 命中危险清单 / 原生 prompt 规则 / 拆不开的脚本仍然弹窗。默认关。
    pub codex_relaxed_shell: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppConfig {
    pub general: GeneralConfig,
    pub channels: ChannelsConfig,
    pub agent_tasks: AgentTasksConfig,
    /// 权限确认设置（宽松模式全局开关等）。
    pub permissions: PermissionsConfig,
    /// 实验性功能开关区（spec D15）。
    pub experimental: ExperimentalConfig,
}

impl AppConfig {
    /// 从指定路径读取；文件缺失或损坏时返回默认配置（容错）。
    pub fn load_from(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// 原子写入指定路径（临时文件 + rename）。
    /// The config holds channel secrets (DingTalk/Feishu AppSecret, Telegram bot token), so the
    /// file is restricted to owner-only (0600) and its directory to 0700 on Unix.
    pub fn save_to(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
            harden_dir(dir);
        }
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
        std::fs::write(&tmp, json.as_bytes())?;
        // Restrict the temp file before rename so the published file is never briefly world-readable.
        harden_file(&tmp);
        std::fs::rename(&tmp, path)?;
        harden_file(path);
        Ok(())
    }

    /// 读取默认位置 `~/.askhuman/config.json`；新位置缺失时回退旧
    /// `~/.humaninloop/config.json`（向后兼容老用户）。
    ///
    /// Secrets are resolved from the OS keychain (see `secrets`): empty config fields are filled
    /// from the keychain; leftover plaintext fields are migrated into the keychain and blanked on
    /// disk. After this, the in-memory secret fields always hold the effective value, so all
    /// downstream readers (channel connect, hot-reload diff) keep working unchanged.
    pub fn load() -> Self {
        let primary = paths::config_file();
        if primary.exists() {
            // Self-heal: tighten perms of a pre-existing file that may have been written with a
            // looser umask (e.g. 0644) before this hardening was added.
            harden_file(&primary);
            if let Some(dir) = primary.parent() {
                harden_dir(dir);
            }
            let mut cfg = Self::load_from(&primary);
            cfg.resolve_secrets_and_migrate(true);
            return cfg;
        }
        // Dev Instance: never fall back to the user's main/legacy config (would pull production bots).
        if !crate::dev_instance::is_dev_instance() {
            let legacy = paths::legacy_config_file();
            if legacy.exists() {
                harden_file(&legacy);
                let mut cfg = Self::load_from(&legacy);
                // Don't persist (would migrate the legacy file to the primary location); just resolve.
                cfg.resolve_secrets_and_migrate(false);
                return cfg;
            }
        }
        let mut cfg = Self::default();
        cfg.resolve_secrets_and_migrate(false);
        cfg
    }

    /// Like `load()` but skips OS-keychain secret resolution/migration entirely.
    ///
    /// Use this from paths that only need non-secret config (UI language, theme, window size,
    /// history limit). They get the on-disk values — in keychain mode the secret fields stay blank,
    /// which is fine since these callers never read them — without ever touching the keychain. This
    /// avoids needless keychain reads on functionally-unrelated commands (e.g. `--version`) and, on
    /// macOS, the password prompt an untrusted/ad-hoc-signed binary would otherwise trigger.
    /// Permissions are still hardened (self-heal), matching `load()`.
    pub fn load_without_secrets() -> Self {
        let primary = paths::config_file();
        if primary.exists() {
            harden_file(&primary);
            if let Some(dir) = primary.parent() {
                harden_dir(dir);
            }
            return Self::load_from(&primary);
        }
        if !crate::dev_instance::is_dev_instance() {
            let legacy = paths::legacy_config_file();
            if legacy.exists() {
                harden_file(&legacy);
                return Self::load_from(&legacy);
            }
        }
        Self::default()
    }

    /// 写入默认位置。Secrets are stripped into the keychain and blanked on disk (plaintext
    /// fallback only when the keychain is unavailable).
    pub fn save(&self) -> std::io::Result<()> {
        let disk = self.persist_secrets_to_keychain();
        disk.save_to(&paths::config_file())
    }

    /// Single pass over the secrets: an empty field is resolved from the keychain; a non-empty
    /// field (leftover plaintext) is migrated into the keychain. The in-memory field always ends
    /// up holding the effective value. When `persist` and at least one plaintext field was
    /// migrated, re-save so the plaintext is blanked on disk.
    fn resolve_secrets_and_migrate(&mut self, persist: bool) {
        let mut migrated = false;
        for spec in &SECRET_SPECS {
            let field = (spec.field)(self);
            if field.is_empty() {
                if let Ok(Some(v)) = secrets::get(spec.account) {
                    *field = v;
                }
            } else {
                let value = field.clone();
                if secrets::set(spec.account, &value).is_ok() {
                    migrated = true;
                }
            }
        }
        if persist && migrated {
            let _ = self.save();
        }
    }

    /// Return a disk copy in which each secret successfully stored in the keychain is blanked;
    /// secrets that can't be stored (keychain unavailable) stay as plaintext (fallback). The
    /// in-memory `self` is left untouched (still holds the resolved values).
    fn persist_secrets_to_keychain(&self) -> AppConfig {
        let mut disk = self.clone();
        for spec in &SECRET_SPECS {
            let field = (spec.field)(&mut disk);
            if field.is_empty() {
                // Empty means "unchanged / keychain-mode / cleared" — leave the keychain as-is
                // (an explicit clear is handled by the settings command via secrets::delete).
                continue;
            }
            let value = field.clone();
            if secrets::set(spec.account, &value).is_ok() {
                field.clear();
            }
        }
        disk
    }
}

/// Restrict a file to owner read/write (0600) on Unix; no-op elsewhere. Best-effort (ignores errors).
#[cfg(unix)]
fn harden_file(path: &Path) {
    harden_to(path, 0o600);
}
#[cfg(not(unix))]
fn harden_file(_path: &Path) {}

/// Restrict a directory to owner-only (0700) on Unix; no-op elsewhere. Best-effort (ignores errors).
#[cfg(unix)]
fn harden_dir(path: &Path) {
    harden_to(path, 0o700);
}
#[cfg(not(unix))]
fn harden_dir(_path: &Path) {}

/// chmod `path` to `mode` only when it differs. Re-chmodding to the same mode still bumps the
/// inode's ctime and emits a filesystem-change event; since `load()` hardens on every read, an
/// unconditional chmod would feed the daemon's config watcher a reload→harden→reload storm.
#[cfg(unix)]
fn harden_to(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.permissions().mode() & 0o777 != mode {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_are_correct() {
        let c = AppConfig::default();
        assert_eq!(c.general.theme, ThemeMode::System);
        assert_eq!(c.general.language, "auto");
        assert!(c.general.always_on_top);
        assert_eq!(c.general.appear_animation, PopupAnimation::Alert);
        assert_eq!(c.general.window_effect, WindowEffect::Blur);
        assert_eq!(c.general.speech_language, "auto");
        assert_eq!(c.general.speech_shortcut, "cmd+d");
        assert_eq!(c.general.history_limit, 200);
        assert_eq!(c.general.menu_bar_icon, MenuBarIconMode::Always);
        assert!(c.general.popup_prewarm);
        assert!(c.channels.popup.enabled);
        assert_eq!(c.channels.popup.width, 560.0);
        assert_eq!(c.channels.popup.height, 620.0);
        assert!(c.channels.popup.remember_size);
        assert!(!c.channels.telegram.enabled);
        assert_eq!(c.channels.telegram.api_base_url, "https://api.telegram.org");
        assert!(!c.channels.dingding.enabled);
        assert!(c.channels.dingding.client_id.is_empty());
        assert!(c.channels.dingding.card_template_id.is_empty());
        assert!(c
            .channels
            .dingding
            .permission_confirm_card_template_id
            .is_empty());
        // 文本附件预览开关默认开启。
        assert!(c.channels.dingding.inline_small_text);
        assert!(c.channels.dingding.convert_text_to_docx);
        // 飞书默认未启用、字段为空、域名为飞书国内。
        assert!(!c.channels.feishu.enabled);
        assert!(c.channels.feishu.app_id.is_empty());
        assert_eq!(c.channels.feishu.base_url, "https://open.feishu.cn");
        // Slack 默认未启用、字段为空。
        assert!(!c.channels.slack.enabled);
        assert!(c.channels.slack.bot_token.is_empty());
        assert!(c.channels.slack.app_token.is_empty());
        assert!(c.channels.slack.user_id.is_empty());
        // 「按需发送」默认关；子开关「自动结束 watch」默认开。
        assert!(!c.channels.auto_activation);
        assert!(c.channels.auto_end_watch);
        assert!(!c.agent_tasks.enabled);
        assert_eq!(c.agent_tasks.permission_prompt, AgentTaskPermission::Ask);
    }

    #[test]
    fn window_effect_solid_round_trips() {
        let json = serde_json::to_string(&WindowEffect::Solid).unwrap();
        assert_eq!(json, "\"solid\"");
        assert_eq!(
            serde_json::from_str::<WindowEffect>(&json).unwrap(),
            WindowEffect::Solid
        );
    }

    #[test]
    fn missing_auto_end_watch_defaults_to_true() {
        // 老配置文件缺 `autoEndWatch` 字段 → 容器级 `#[serde(default)]` 回退到 ChannelsConfig::default() 的 true。
        let json = r#"{"channels":{"autoActivation":true}}"#;
        let c: AppConfig = serde_json::from_str(json).unwrap();
        assert!(c.channels.auto_activation);
        assert!(c.channels.auto_end_watch);
    }

    #[test]
    fn missing_auto_activation_defaults_to_false_but_explicit_true_is_preserved() {
        let missing = r#"{"channels":{}}"#;
        let c: AppConfig = serde_json::from_str(missing).unwrap();
        assert!(!c.channels.auto_activation);

        let explicit_true = r#"{"channels":{"autoActivation":true}}"#;
        let c: AppConfig = serde_json::from_str(explicit_true).unwrap();
        assert!(c.channels.auto_activation);
    }

    #[test]
    fn missing_file_returns_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let c = AppConfig::load_from(&path);
        assert!(c.general.always_on_top);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"general":{"theme":"dark"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.theme, ThemeMode::Dark);
        // 缺失字段走默认
        assert!(c.general.always_on_top);
        assert!(c.channels.popup.enabled);
    }

    #[test]
    fn unknown_fields_ignored() {
        // 旧版 markdownRenderer 字段应被忽略而非报错
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(
            &path,
            r#"{"general":{"markdownRenderer":"webview","theme":"light"}}"#,
        )
        .unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.theme, ThemeMode::Light);
    }

    #[test]
    fn round_trip_save_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut c = AppConfig::default();
        c.general.theme = ThemeMode::Dark;
        c.channels.telegram.enabled = true;
        c.channels.telegram.chat_id = "12345".to_string();
        c.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.general.theme, ThemeMode::Dark);
        assert!(loaded.channels.telegram.enabled);
        assert_eq!(loaded.channels.telegram.chat_id, "12345");
    }

    #[test]
    fn menu_bar_icon_preserves_explicit_values_and_defaults_to_always() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");

        std::fs::write(&path, r#"{"general":{"menuBarIcon":"off"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.menu_bar_icon, MenuBarIconMode::Off);

        std::fs::write(&path, r#"{"general":{"menuBarIcon":"active"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.menu_bar_icon, MenuBarIconMode::Active);

        std::fs::write(&path, r#"{"general":{"menuBarIcon":"always"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.menu_bar_icon, MenuBarIconMode::Always);

        // A legacy config without the field adopts the new default.
        std::fs::write(&path, r#"{"general":{"theme":"dark"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.menu_bar_icon, MenuBarIconMode::Always);

        // An invalid enum value makes the config fall back to the complete default.
        std::fs::write(&path, r#"{"general":{"menuBarIcon":"bogus"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.menu_bar_icon, MenuBarIconMode::Always);
    }

    #[test]
    fn daemon_lifecycle_parses_lowercase_and_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, r#"{"general":{"daemonLifecycle":"keepalive"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.daemon_lifecycle, DaemonLifecycleMode::KeepAlive);
        // 缺字段 → 默认 Activity（旧配置零影响）。
        std::fs::write(&path, r#"{"general":{"theme":"dark"}}"#).unwrap();
        let c = AppConfig::load_from(&path);
        assert_eq!(c.general.daemon_lifecycle, DaemonLifecycleMode::Activity);
    }

    #[cfg(unix)]
    #[test]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        AppConfig::default().save_to(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "config file must be owner read/write only");
    }
}
