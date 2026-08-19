//! Agent lifecycle tracking integration. Automatic integrations own their user-level lifecycle
//! hooks; Pi carries the capability in its shared managed extension. Tracking can be disabled
//! inside an active integration, while `Mode::None` never retains AskHuman lifecycle artifacts.
//!
//! 设计要点：
//! - The timeout hook (`askhuman-timeout.sh`) remains independent. Lifecycle events use the
//!   `__agent-hook` marker, while supported Stop events use the single shared `__stop-hook` handler
//!   that can also perform end confirmation. Other hooks and JSONC/TOML formatting are preserved.
//! - Claude/Cursor 是 JSON 配置（无信任）；Codex 是 `~/.codex/hooks.json` 定义 + `~/.codex/config.toml`
//!   `[hooks.state]` 写信任哈希（复刻 codex `version_for_toml`，见 `demo/.../codex-trust.cjs` 与
//!   FINDINGS §6.2）。Codex 无 SessionEnd 事件。
//! - 去重（Cursor 双触发）由 reporter 运行时按 env 判定真实家族解决，故三家可同时安装、互不影响。

use crate::agents::AgentKind;
use crate::paths;
use anyhow::{anyhow, Context, Result};
use jsonc_parser::cst::{CstNode, CstRootNode};
use jsonc_parser::json;
use jsonc_parser::ParseOptions;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// 识别本功能注入条目的命令标记。
pub const MARKER: &str = "__agent-hook";

/// Lifecycle preference and on-disk state aggregated for an Agent integration card.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleStatus {
    /// Effective preference while the integration is active. Always false in None mode.
    pub enabled: bool,
    /// Whether the user/update flow has persisted an explicit preference.
    pub preference_configured: bool,
    /// Whether at least one lifecycle event contains an AskHuman entry.
    pub installed: bool,
    /// Whether an installed artifact is stale or incomplete.
    pub outdated: bool,
    /// Whether lifecycle hooks are supported on the current platform.
    pub supported: bool,
    /// The current mode/preferences and the actual artifact disagree or the artifact is outdated.
    pub needs_update: bool,
    /// A lifecycle artifact exists although the current mode/preferences require its removal.
    pub cleanup_required: bool,
}

#[derive(Debug, Clone, Copy)]
struct DiskLifecycleStatus {
    installed: bool,
    outdated: bool,
    supported: bool,
}

#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
struct LifecyclePreferences {
    #[serde(default)]
    claude: Option<bool>,
    #[serde(default)]
    codex: Option<bool>,
    #[serde(default)]
    cursor: Option<bool>,
    #[serde(default)]
    grok: Option<bool>,
    #[serde(default)]
    pi: Option<bool>,
}

impl LifecyclePreferences {
    fn get(&self, kind: AgentKind) -> Option<bool> {
        match kind {
            AgentKind::Claude => self.claude,
            AgentKind::Codex => self.codex,
            AgentKind::Cursor => self.cursor,
            AgentKind::Grok => self.grok,
            AgentKind::Pi => self.pi,
        }
    }

    fn set(&mut self, kind: AgentKind, value: bool) {
        let slot = match kind {
            AgentKind::Claude => &mut self.claude,
            AgentKind::Codex => &mut self.codex,
            AgentKind::Cursor => &mut self.cursor,
            AgentKind::Grok => &mut self.grok,
            AgentKind::Pi => &mut self.pi,
        };
        *slot = Some(value);
    }
}

/// 某家族要注册的事件：(配置文件里的事件键, 归一化 lifecycle 事件)。
/// Codex 额外需要 snake_case 标签用于信任键/身份（见 `codex_label`）。
fn events(kind: AgentKind) -> &'static [(&'static str, &'static str)] {
    match kind {
        // Claude：SessionStart/UserPromptSubmit/Stop/SessionEnd。
        // 另加 StopFailure（API 错误结束回合时代替 Stop，官方文档明确 Stop 此时不触发）→ turn-end；
        // Pre/PostToolUse → activity（回合内工具调用心跳，喂「工作中兜底超时」，避免长回合误判空闲）。
        AgentKind::Claude => &[
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "turn-start"),
            ("PreToolUse", "activity"),
            ("PostToolUse", "activity"),
            ("Stop", "turn-end"),
            ("StopFailure", "turn-end"),
            ("SessionEnd", "session-end"),
        ],
        // Codex：无 SessionEnd / 无 StopFailure / 无 Notification（FINDINGS §6）。Pre/PostToolUse → activity。
        AgentKind::Codex => &[
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "turn-start"),
            ("PreToolUse", "activity"),
            ("PostToolUse", "activity"),
            ("Stop", "turn-end"),
        ],
        // Cursor：原生 camelCase 事件。preToolUse/postToolUse → activity。
        AgentKind::Cursor => &[
            ("sessionStart", "session-start"),
            ("beforeSubmitPrompt", "turn-start"),
            ("preToolUse", "activity"),
            ("postToolUse", "activity"),
            ("stop", "turn-end"),
            ("sessionEnd", "session-end"),
        ],
        // Grok：PascalCase 事件，与 Claude 同构且事件最全（含 StopFailure + SessionEnd）。
        // 落点 `~/.grok/hooks/*.json`（全局恒受信任，无需 Codex 那种信任哈希）。
        AgentKind::Grok => &[
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "turn-start"),
            ("PreToolUse", "activity"),
            ("PostToolUse", "activity"),
            ("Stop", "turn-end"),
            ("StopFailure", "turn-end"),
            ("SessionEnd", "session-end"),
        ],
        // Pi emits lifecycle events from the managed TypeScript extension, not JSON hooks.
        AgentKind::Pi => &[],
    }
}

/// Explicit timeout for PreToolUse interjection waits, in seconds.
pub const PRE_TOOL_USE_TIMEOUT_SECS: u64 = 86400;

/// Return the explicit lifecycle timeout for an event. Shared Stop handlers are handled separately.
fn event_timeout(kind: AgentKind, event_key: &str) -> Option<u64> {
    match (kind, event_key) {
        (AgentKind::Claude, "PreToolUse")
        | (AgentKind::Codex, "PreToolUse")
        | (AgentKind::Cursor, "preToolUse") => Some(PRE_TOOL_USE_TIMEOUT_SECS),
        _ => None,
    }
}

/// Codex 事件键（PascalCase）→ 信任用 snake_case 标签（hooks/src/lib.rs::hook_event_key_label）。
fn codex_label(event_key: &str) -> Option<&'static str> {
    match event_key {
        "SessionStart" => Some("session_start"),
        "UserPromptSubmit" => Some("user_prompt_submit"),
        "PreToolUse" => Some("pre_tool_use"),
        "PostToolUse" => Some("post_tool_use"),
        "Stop" => Some("stop"),
        _ => None,
    }
}

pub fn supported() -> bool {
    true
}

fn load_preferences() -> LifecyclePreferences {
    std::fs::read(paths::lifecycle_preferences_file())
        .ok()
        .map(|bytes| parse_preferences(&bytes))
        .unwrap_or_default()
}

fn parse_preferences(bytes: &[u8]) -> LifecyclePreferences {
    serde_json::from_slice(bytes).unwrap_or_default()
}

fn save_preferences(preferences: &LifecyclePreferences) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(preferences)?;
    super::hook_edit::atomic_write(&paths::lifecycle_preferences_file(), &bytes)
}

pub(crate) fn preference(kind: AgentKind) -> Option<bool> {
    load_preferences().get(kind)
}

pub(crate) fn target_for_kind(kind: AgentKind) -> super::agent_rules::AgentTarget {
    match kind {
        AgentKind::Claude => super::agent_rules::AgentTarget::ClaudeCode,
        AgentKind::Codex => super::agent_rules::AgentTarget::Codex,
        AgentKind::Cursor => super::agent_rules::AgentTarget::Cursor,
        AgentKind::Grok => super::agent_rules::AgentTarget::Grok,
        AgentKind::Pi => super::agent_rules::AgentTarget::Pi,
    }
}

fn aggregate_status(
    mode: super::agent_mode::Mode,
    preference: Option<bool>,
    disk: DiskLifecycleStatus,
) -> LifecycleStatus {
    let enabled = mode != super::agent_mode::Mode::None && preference.unwrap_or(true);
    let cleanup_required = disk.supported && !enabled && disk.installed;
    let needs_update = disk.supported
        && if enabled {
            !disk.installed || disk.outdated
        } else {
            disk.installed
        };
    LifecycleStatus {
        enabled,
        preference_configured: preference.is_some(),
        installed: disk.installed,
        outdated: disk.outdated,
        supported: disk.supported,
        needs_update,
        cleanup_required,
    }
}

pub(crate) fn status_for_mode(kind: AgentKind, mode: super::agent_mode::Mode) -> LifecycleStatus {
    aggregate_status(mode, preference(kind), disk_status(kind))
}

/// 是否有任意一家 agent 已开启生命周期追踪（即至少一家装了本功能的 lifecycle hook）。
/// 用于「未开启任何追踪」时隐藏 Agent 状态相关入口（托盘菜单等）。
pub fn any_installed() -> bool {
    if !supported() {
        return false;
    }
    [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Cursor,
        AgentKind::Grok,
        AgentKind::Pi,
    ]
    .iter()
    .any(|k| disk_status(*k).installed)
}

/// Whether lifecycle tracking is installed. A shared Stop handler counts only when it carries the
/// `track` flag; a confirmation-only handler does not.
pub(crate) fn tracking_installed(kind: AgentKind) -> bool {
    if !supported() {
        return false;
    }
    if kind == AgentKind::Pi {
        return super::pi_extension::status().lifecycle;
    }
    let (path, shape) = match kind {
        AgentKind::Claude => (paths::claude_settings_json(), Shape::Nested),
        AgentKind::Codex => (paths::codex_hooks_json(), Shape::Nested),
        AgentKind::Cursor => (paths::cursor_hooks_json(), Shape::Flat),
        AgentKind::Grok => (paths::grok_hooks_json(), Shape::Nested),
        AgentKind::Pi => unreachable!(),
    };
    let Some(root) = read_value(&path) else {
        return false;
    };
    root.get("hooks")
        .and_then(Value::as_object)
        .is_some_and(|hooks| {
            hooks.values().any(|groups| {
                groups.as_array().is_some_and(|groups| {
                    groups
                        .iter()
                        .any(|group| elem_tracks_lifecycle(group, shape))
                })
            })
        })
}

/// 当前可执行文件绝对路径（hook 命令调用它）。
fn exe_path() -> Result<String> {
    let p = std::env::current_exe().context("failed to resolve current exe path")?;
    Ok(p.to_string_lossy().to_string())
}

/// hook 命令字符串：`"<exe>" __agent-hook <agent> <lifecycle-event>`。
fn hook_command(
    exe: &str,
    kind: AgentKind,
    event_key: &str,
    lc_event: &str,
    stop_confirm: bool,
) -> String {
    if is_stop_event(kind, event_key) {
        return super::agent_stop::hook_command_for(exe, kind, true, stop_confirm);
    }
    format!("\"{}\" {} {} {}", exe, MARKER, kind.as_str(), lc_event)
}

fn windows_hook_command(
    exe: &str,
    kind: AgentKind,
    event_key: &str,
    lc_event: &str,
    stop_confirm: bool,
) -> String {
    if is_stop_event(kind, event_key) {
        return super::agent_stop::windows_hook_command_for(exe, kind, true, stop_confirm);
    }
    super::hook_edit::powershell_command(exe, &[MARKER, kind.as_str(), lc_event])
}

fn is_stop_event(kind: AgentKind, event_key: &str) -> bool {
    kind != AgentKind::Grok
        && matches!((kind, event_key), (AgentKind::Cursor, "stop") | (_, "Stop"))
}

// ===== Public status, preference, installation, and removal API =====

pub fn status(kind: AgentKind) -> LifecycleStatus {
    let mode = super::agent_mode::current(target_for_kind(kind));
    status_for_mode(kind, mode)
}

fn disk_status(kind: AgentKind) -> DiskLifecycleStatus {
    if !supported() {
        return DiskLifecycleStatus {
            installed: false,
            outdated: false,
            supported: false,
        };
    }
    match kind {
        AgentKind::Claude => json_status(kind, &paths::claude_settings_json(), Shape::Nested),
        AgentKind::Cursor => json_status(kind, &paths::cursor_hooks_json(), Shape::Flat),
        AgentKind::Codex => codex_status(),
        // Grok：全局 hooks 恒受信任，纯 JSON 状态即可（无 Codex 那种信任哈希校验）。
        AgentKind::Grok => json_status(kind, &paths::grok_hooks_json(), Shape::Nested),
        AgentKind::Pi => {
            let extension = super::pi_extension::status();
            DiskLifecycleStatus {
                installed: extension.lifecycle,
                outdated: extension.lifecycle && extension.outdated,
                supported: true,
            }
        }
    }
}

/// Change the lifecycle preference inside an active automatic integration. Turning the preference
/// off in None mode is accepted as an idempotent cleanup; turning it on would create the invalid
/// `None + lifecycle` state and is rejected.
pub fn set_enabled(kind: AgentKind, value: bool) -> Result<String> {
    let _lock = super::mutation_lock::IntegrationMutationLock::acquire()?;
    let mode = super::agent_mode::current(target_for_kind(kind));
    if value && mode == super::agent_mode::Mode::None {
        return Err(anyhow!(
            "enable an automatic Agent integration before enabling lifecycle tracking"
        ));
    }

    let original = load_preferences();
    let mut preferences = original.clone();
    preferences.set(kind, value);
    save_preferences(&preferences)?;
    let result = reconcile_artifact_unlocked(kind, mode, value);
    if let Err(error) = result {
        let _ = save_preferences(&original);
        return Err(error);
    }
    Ok(message(if value {
        "cmd.lifecycleInstalled"
    } else {
        "cmd.lifecycleRemoved"
    }))
}

pub fn install(kind: AgentKind) -> Result<String> {
    set_enabled(kind, true)
}

pub(crate) fn install_unlocked(kind: AgentKind) -> Result<String> {
    let exe = exe_path()?;
    match kind {
        AgentKind::Claude => {
            json_install(kind, &exe, &paths::claude_settings_json(), Shape::Nested)?
        }
        AgentKind::Cursor => json_install(kind, &exe, &paths::cursor_hooks_json(), Shape::Flat)?,
        AgentKind::Codex => codex_install(&exe)?,
        AgentKind::Grok => json_install(kind, &exe, &paths::grok_hooks_json(), Shape::Nested)?,
        AgentKind::Pi => super::pi_extension::set_lifecycle_enabled(true)?,
    }
    Ok(message("cmd.lifecycleInstalled"))
}

pub fn uninstall(kind: AgentKind) -> Result<String> {
    set_enabled(kind, false)
}

/// Reconcile lifecycle as part of an Agent mode mutation. When `adopt_default` is true, an active
/// legacy integration with no stored preference adopts the new default-on preference. None mode
/// always removes the artifact while preserving the stored preference for a later re-integration.
pub(crate) fn reconcile_unlocked(
    kind: AgentKind,
    mode: super::agent_mode::Mode,
    adopt_default: bool,
) -> Result<()> {
    let original = load_preferences();
    let mut preferences = original.clone();
    let mut changed = false;
    if mode != super::agent_mode::Mode::None && preferences.get(kind).is_none() && adopt_default {
        preferences.set(kind, true);
        save_preferences(&preferences)?;
        changed = true;
    }
    let desired = mode != super::agent_mode::Mode::None && preferences.get(kind).unwrap_or(true);
    if let Err(error) = reconcile_artifact_unlocked(kind, mode, desired) {
        if changed {
            let _ = save_preferences(&original);
        }
        return Err(error);
    }
    Ok(())
}

fn reconcile_artifact_unlocked(
    kind: AgentKind,
    mode: super::agent_mode::Mode,
    desired: bool,
) -> Result<()> {
    if mode != super::agent_mode::Mode::None && desired {
        install_unlocked(kind)?;
    } else {
        uninstall_unlocked(kind)?;
    }
    Ok(())
}

pub(crate) fn uninstall_unlocked(kind: AgentKind) -> Result<String> {
    match kind {
        AgentKind::Claude => json_uninstall(kind, &paths::claude_settings_json(), Shape::Nested)?,
        AgentKind::Cursor => json_uninstall(kind, &paths::cursor_hooks_json(), Shape::Flat)?,
        AgentKind::Codex => codex_uninstall()?,
        AgentKind::Grok => json_uninstall(kind, &paths::grok_hooks_json(), Shape::Nested)?,
        AgentKind::Pi => super::pi_extension::set_lifecycle_enabled(false)?,
    }
    super::agent_stop::reconcile_current_mode_unlocked(kind)?;
    Ok(message("cmd.lifecycleRemoved"))
}

fn message(key: &'static str) -> String {
    let lang = crate::i18n::Lang::current();
    crate::i18n::tr(lang, key).to_string()
}

// ===== JSON（Claude=Nested / Cursor=Flat） =====

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    /// `{ "Event": [ { "hooks": [ { "type":"command", "command": ... } ] } ] }`（Claude/Codex）。
    Nested,
    /// `{ "event": [ { "command": ... } ] }`（Cursor，含顶层 `version`）。
    Flat,
}

/// 元素（事件数组的一项）是否含本功能命令（按 shape 解析）。
fn elem_has_marker(elem: &Value, shape: Shape) -> bool {
    match shape {
        Shape::Nested => elem
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|arr| arr.iter().any(|h| cmd_has_marker(h.get("command"))))
            .unwrap_or(false),
        Shape::Flat => cmd_has_marker(elem.get("command")),
    }
}

fn cmd_has_marker(cmd: Option<&Value>) -> bool {
    cmd.and_then(|c| c.as_str())
        .map(|c| c.contains(MARKER))
        .unwrap_or(false)
}

fn elem_node_has_marker(node: &CstNode, shape: Shape, include_stop: bool) -> bool {
    node.to_serde_value()
        .map(|v| {
            elem_has_marker(&v, shape)
                || (include_stop && elem_has_command_marker(&v, shape, super::agent_stop::MARKER))
        })
        .unwrap_or(false)
}

fn elem_has_command_marker(elem: &Value, shape: Shape, marker: &str) -> bool {
    match shape {
        Shape::Nested => elem
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|handlers| {
                handlers.iter().any(|handler| {
                    ["command", "commandWindows"].iter().any(|field| {
                        handler
                            .get(*field)
                            .and_then(Value::as_str)
                            .is_some_and(|command| command.contains(marker))
                    })
                })
            }),
        Shape::Flat => ["command", "commandWindows"].iter().any(|field| {
            elem.get(*field)
                .and_then(Value::as_str)
                .is_some_and(|command| command.contains(marker))
        }),
    }
}

fn elem_tracks_lifecycle(elem: &Value, shape: Shape) -> bool {
    if elem_has_marker(elem, shape) {
        return true;
    }
    let has_track = |command: &str| {
        command.contains(super::agent_stop::MARKER)
            && command.split_whitespace().any(|part| part == "track")
    };
    match shape {
        Shape::Nested => elem
            .get("hooks")
            .and_then(Value::as_array)
            .is_some_and(|handlers| {
                handlers.iter().any(|handler| {
                    handler
                        .get("command")
                        .and_then(Value::as_str)
                        .is_some_and(has_track)
                })
            }),
        Shape::Flat => elem
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(has_track),
    }
}

fn read_value(path: &std::path::Path) -> Option<Value> {
    let text = std::fs::read_to_string(path).ok()?;
    jsonc_parser::parse_to_serde_value(&text, &ParseOptions::default()).ok()
}

fn json_status(kind: AgentKind, path: &std::path::Path, shape: Shape) -> DiskLifecycleStatus {
    let exe = exe_path().unwrap_or_default();
    let Some(root) = read_value(path) else {
        return DiskLifecycleStatus {
            installed: false,
            outdated: false,
            supported: true,
        };
    };
    let (any, complete) = json_presence_with_stop(
        kind,
        &exe,
        &root,
        shape,
        super::agent_stop::active_in_current_mode(kind),
    );
    DiskLifecycleStatus {
        installed: any,
        outdated: any && !complete,
        supported: true,
    }
}

/// 已安装 / 完整性判定（纯函数，供单测）：`any`＝至少一个事件含本功能条目；
/// `complete`＝每个事件都恰好是期望形态（命令逐字一致 + PreToolUse 带 timeout=86400）。
/// Incomplete artifacts, such as an older entry without a timeout, become `outdated` and are
/// repaired only after the user chooses Update in the Agent integration.
fn json_presence(kind: AgentKind, exe: &str, root: &Value, shape: Shape) -> (bool, bool) {
    json_presence_with_stop(kind, exe, root, shape, false)
}

fn json_presence_with_stop(
    kind: AgentKind,
    exe: &str,
    root: &Value,
    shape: Shape,
    stop_confirm: bool,
) -> (bool, bool) {
    let hooks = root.get("hooks");
    let mut any = false;
    let mut complete = true;
    for (event_key, lc) in events(kind) {
        let want = hook_command(exe, kind, event_key, lc, stop_confirm);
        let want_windows = (kind == AgentKind::Codex)
            .then(|| windows_hook_command(exe, kind, event_key, lc, stop_confirm));
        let want_timeout = if is_stop_event(kind, event_key) {
            Some(super::agent_stop::TIMEOUT_SECS)
        } else {
            event_timeout(kind, event_key)
        };
        let want_unlimited_loop = kind == AgentKind::Cursor && *event_key == "stop";
        let arr = hooks
            .and_then(|h| h.get(event_key))
            .and_then(|a| a.as_array());
        let has_ours = arr
            .map(|a| a.iter().any(|e| elem_tracks_lifecycle(e, shape)))
            .unwrap_or(false);
        let has_exact = arr
            .map(|a| {
                a.iter().any(|e| {
                    elem_matches(
                        e,
                        shape,
                        &want,
                        want_windows.as_deref(),
                        want_timeout,
                        want_unlimited_loop,
                    )
                })
            })
            .unwrap_or(false);
        if has_ours {
            any = true;
        }
        if !has_exact {
            complete = false;
        }
    }
    (any, complete)
}

/// 元素是否恰好为期望形态：命令逐字一致，且（要求显式 timeout 的事件）timeout 一致。
/// 用于 outdated 判定：路径变化 / 旧版缺 timeout 都会触发幂等重装。
fn elem_matches(
    elem: &Value,
    shape: Shape,
    want: &str,
    want_windows: Option<&str>,
    want_timeout: Option<u64>,
    want_unlimited_loop: bool,
) -> bool {
    let timeout_ok = |h: &Value| match want_timeout {
        Some(t) => h.get("timeout").and_then(|v| v.as_u64()) == Some(t),
        None => true,
    };
    match shape {
        Shape::Nested => elem
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter().any(|h| {
                    h.get("command").and_then(|c| c.as_str()) == Some(want)
                        && want_windows.is_none_or(|want_windows| {
                            h.get("commandWindows").and_then(Value::as_str) == Some(want_windows)
                        })
                        && timeout_ok(h)
                })
            })
            .unwrap_or(false),
        Shape::Flat => {
            elem.get("command").and_then(|c| c.as_str()) == Some(want)
                && timeout_ok(elem)
                && (!want_unlimited_loop || elem.get("loop_limit").is_some_and(Value::is_null))
        }
    }
}

fn json_install(kind: AgentKind, exe: &str, path: &std::path::Path, shape: Shape) -> Result<()> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".to_string());
    let updated = apply_json_install_with_stop(
        kind,
        exe,
        &text,
        shape,
        super::agent_stop::active_in_current_mode(kind),
    )?;
    write_text(path, &updated)
}

fn json_uninstall(kind: AgentKind, path: &std::path::Path, shape: Shape) -> Result<()> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let updated = apply_json_uninstall(kind, &text, shape)?;
    write_text(path, &updated)
}

/// 在 JSON 文本中插入/更新本功能各事件条目（CST 保留格式）。仅触碰本功能条目。
fn apply_json_install(kind: AgentKind, exe: &str, text: &str, shape: Shape) -> Result<String> {
    apply_json_install_with_stop(kind, exe, text, shape, false)
}

fn apply_json_install_with_stop(
    kind: AgentKind,
    exe: &str,
    text: &str,
    shape: Shape,
    stop_confirm: bool,
) -> Result<String> {
    let source = if text.trim().is_empty() { "{}" } else { text };
    let root = CstRootNode::parse(source, &ParseOptions::default())
        .map_err(|e| anyhow!("解析配置失败，已中止（不覆盖原文件）：{e}"))?;
    let root_obj = root
        .object_value_or_create()
        .ok_or_else(|| anyhow!("配置根不是 JSON 对象，已中止"))?;

    // Cursor hooks.json 需要顶层 version=1。
    if shape == Shape::Flat && root_obj.get("version").is_none() {
        root_obj.append("version", json!(1));
    }

    let hooks = root_obj
        .object_value_or_create("hooks")
        .ok_or_else(|| anyhow!("配置的 'hooks' 不是对象，已中止"))?;

    for (event_key, lc) in events(kind) {
        let command = hook_command(exe, kind, event_key, lc, stop_confirm);
        let cmd = command.as_str();
        let command_windows = (kind == AgentKind::Codex)
            .then(|| windows_hook_command(exe, kind, event_key, lc, stop_confirm));
        let cmd_windows = command_windows.as_deref().unwrap_or(cmd);
        // Stop confirmation and PreToolUse interjection waits both need a 24-hour hook timeout.
        let timeout = if is_stop_event(kind, event_key) {
            Some(super::agent_stop::TIMEOUT_SECS)
        } else {
            event_timeout(kind, event_key)
        };
        let entry = match (shape, timeout) {
            (Shape::Nested, Some(t)) if kind == AgentKind::Codex => json!({
                "hooks": [ {
                    "type": "command",
                    "command": cmd,
                    "commandWindows": cmd_windows,
                    "timeout": t
                } ]
            }),
            (Shape::Nested, None) if kind == AgentKind::Codex => json!({
                "hooks": [ {
                    "type": "command",
                    "command": cmd,
                    "commandWindows": cmd_windows
                } ]
            }),
            (Shape::Nested, Some(t)) => {
                json!({ "hooks": [ { "type": "command", "command": cmd, "timeout": t } ] })
            }
            (Shape::Nested, None) => {
                json!({ "hooks": [ { "type": "command", "command": cmd } ] })
            }
            (Shape::Flat, Some(t)) if kind == AgentKind::Cursor && *event_key == "stop" => {
                json!({ "command": cmd, "timeout": t, "loop_limit": null })
            }
            (Shape::Flat, Some(t)) => json!({ "command": cmd, "timeout": t }),
            (Shape::Flat, None) => json!({ "command": cmd }),
        };
        let arr = hooks
            .array_value_or_create(event_key)
            .ok_or_else(|| anyhow!("配置的 '{event_key}' 不是数组，已中止"))?;
        let mut replaced = false;
        for e in arr.elements() {
            if !elem_node_has_marker(&e, shape, is_stop_event(kind, event_key)) {
                continue;
            }
            if !replaced {
                if let Some(obj) = e.as_object() {
                    obj.replace_with(entry.clone());
                    replaced = true;
                    continue;
                }
            }
            e.remove();
        }
        if !replaced {
            arr.ensure_multiline();
            arr.append(entry);
        }
    }
    Ok(root.to_string())
}

/// 在 JSON 文本中移除本功能各事件条目；事件数组变空则删除该键。仅触碰本功能条目。
fn apply_json_uninstall(kind: AgentKind, text: &str, shape: Shape) -> Result<String> {
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|e| anyhow!("解析配置失败，已中止（不覆盖原文件）：{e}"))?;
    let Some(root_obj) = root.object_value() else {
        return Ok(root.to_string());
    };
    let Some(hooks) = root_obj.object_value("hooks") else {
        return Ok(root.to_string());
    };
    // 事件键从同文本的 serde 解析取（避免依赖 CST 的属性枚举 API）；逐个清理本功能元素，空数组删键。
    let parsed: Option<Value> =
        jsonc_parser::parse_to_serde_value(text, &ParseOptions::default()).ok();
    let keys: Vec<String> = parsed
        .as_ref()
        .and_then(|v| v.get("hooks"))
        .and_then(|h| h.as_object())
        .map(|o| o.keys().cloned().collect::<Vec<String>>())
        .unwrap_or_default();
    for key in keys {
        if let Some(arr) = hooks.array_value(&key) {
            for e in arr.elements() {
                if elem_node_has_marker(&e, shape, is_stop_event(kind, &key)) {
                    e.remove();
                }
            }
            if arr.elements().is_empty() {
                if let Some(prop) = hooks.get(&key) {
                    prop.remove();
                }
            }
        }
    }
    Ok(root.to_string())
}

// ===== Codex（hooks.json 定义 + config.toml 信任哈希） =====

fn codex_install(exe: &str) -> Result<()> {
    // 1) 写 ~/.codex/hooks.json（Nested shape，与 Claude 同构）。
    let path = paths::codex_hooks_json();
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
    let updated = apply_json_install_with_stop(
        AgentKind::Codex,
        exe,
        &text,
        Shape::Nested,
        super::agent_stop::active_in_current_mode(AgentKind::Codex),
    )?;
    write_text(&path, &updated)?;
    if let Err(error) = super::agent_permission::reconcile_codex_trust(
        &text,
        &updated,
        &[MARKER, super::agent_stop::MARKER],
    ) {
        let _ = write_text(&path, &text);
        return Err(error);
    }
    Ok(())
}

fn codex_uninstall() -> Result<()> {
    let path = paths::codex_hooks_json();
    if let Ok(text) = std::fs::read_to_string(&path) {
        let updated = apply_json_uninstall(AgentKind::Codex, &text, Shape::Nested)?;
        write_text(&path, &updated)?;
        if let Err(error) = super::agent_permission::reconcile_codex_trust(&text, &updated, &[]) {
            let _ = write_text(&path, &text);
            return Err(error);
        }
    }
    Ok(())
}

fn codex_status() -> DiskLifecycleStatus {
    let base = json_status(AgentKind::Codex, &paths::codex_hooks_json(), Shape::Nested);
    if !base.installed {
        return base;
    }
    // 信任校验：每个期望键的 trusted_hash 必须存在且匹配。
    let trust_ok = match codex_trust_entries(&paths::codex_hooks_json()) {
        Ok(entries) if !entries.is_empty() => {
            let have = read_codex_trust();
            entries
                .iter()
                .all(|(k, h)| have.get(k).map(|v| v == h).unwrap_or(false))
        }
        _ => false,
    };
    DiskLifecycleStatus {
        installed: true,
        outdated: base.outdated || !trust_ok,
        supported: true,
    }
}

/// 由 hooks.json 实际结构计算本功能各 handler 的 (信任键, trusted_hash)。
/// 信任键 = `<abs hooks.json>:<snake_label>:<group_index>:<handler_index>`。
fn codex_trust_entries(hooks_json: &std::path::Path) -> Result<Vec<(String, String)>> {
    // 信任键前缀必须与 codex 构造的 `hooks_config_folder/hooks.json` 显示路径逐字一致。
    // codex 用 `$CODEX_HOME(=~/.codex)/hooks.json` 且不做符号链接规范化，故这里也用原始绝对路径
    // （不 canonicalize，以免解析符号链接后与 codex 的路径分叉）。
    let abs_str = hooks_json.to_string_lossy().to_string();
    let Some(root) = read_value(hooks_json) else {
        return Ok(Vec::new());
    };
    let Some(hooks) = root.get("hooks").and_then(|h| h.as_object()) else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (event_key, groups) in hooks {
        let Some(label) = codex_label(event_key) else {
            continue;
        };
        let Some(groups) = groups.as_array() else {
            continue;
        };
        for (gi, group) in groups.iter().enumerate() {
            let Some(handlers) = group.get("hooks").and_then(|h| h.as_array()) else {
                continue;
            };
            for (hi, handler) in handlers.iter().enumerate() {
                #[cfg(windows)]
                let cmd = handler
                    .get("commandWindows")
                    .or_else(|| handler.get("command"))
                    .and_then(Value::as_str);
                #[cfg(not(windows))]
                let cmd = handler.get("command").and_then(Value::as_str);
                let is_command = handler.get("type").and_then(|t| t.as_str()) == Some("command");
                let Some(cmd) = cmd else { continue };
                let owned = ["command", "commandWindows"].iter().any(|field| {
                    handler
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|command| {
                            command.contains(MARKER) || command.contains(super::agent_stop::MARKER)
                        })
                });
                if !is_command || !owned {
                    continue;
                }
                let key = format!("{abs_str}:{label}:{gi}:{hi}");
                // 按条目实际 timeout 计算（缺省 600、下限 1——复刻 codex discovery
                // `timeout_sec.unwrap_or(600).max(1)` 的归一化）；PreToolUse 写入 86400。
                let timeout = handler
                    .get("timeout")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(600)
                    .max(1);
                let hash = codex_trusted_hash(label, cmd, timeout);
                out.push((key, hash));
            }
        }
    }
    Ok(out)
}

/// 复刻 codex `version_for_toml(NormalizedHookIdentity)`：
/// `"sha256:" + hex(sha256(canonical_compact_json(identity)))`。
/// identity = { event_name, hooks:[{type:"command", command, timeout, async:false}] }
/// （我们的事件 matcher 恒 None → 省略；timeout 按条目实际值——codex 归一化为
/// `unwrap_or(600).max(1)`，PreToolUse 写 86400、其余默认 600；async 默认 false）。
fn codex_trusted_hash(label: &str, command: &str, timeout_sec: u64) -> String {
    let identity = serde_json::json!({
        "event_name": label,
        "hooks": [ {
            "type": "command",
            "command": command,
            "timeout": timeout_sec,
            "async": false,
        } ],
    });
    let mut buf = String::new();
    canonical_compact(&identity, &mut buf);
    let mut hasher = Sha256::new();
    hasher.update(buf.as_bytes());
    let hex = hasher.finalize();
    format!("sha256:{}", hex_encode(&hex))
}

/// 递归输出「键排序 + 紧凑」的规范化 JSON（与 codex `canonical_json` 一致，不依赖 Map 排序实现）。
fn canonical_compact(v: &Value, out: &mut String) {
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).unwrap_or_default());
                out.push(':');
                canonical_compact(&map[*k], out);
            }
            out.push('}');
        }
        Value::Array(arr) => {
            out.push('[');
            for (i, e) in arr.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical_compact(e, out);
            }
            out.push(']');
        }
        other => out.push_str(&serde_json::to_string(other).unwrap_or_default()),
    }
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// 读取 config.toml 中现有的 `[hooks.state.<key>] trusted_hash`（key → hash）。
fn read_codex_trust() -> std::collections::HashMap<String, String> {
    use toml_edit::DocumentMut;
    let mut map = std::collections::HashMap::new();
    let Ok(text) = std::fs::read_to_string(paths::codex_config_toml()) else {
        return map;
    };
    let Ok(doc) = text.parse::<DocumentMut>() else {
        return map;
    };
    if let Some(state) = doc
        .get("hooks")
        .and_then(|h| h.as_table())
        .and_then(|h| h.get("state"))
        .and_then(|s| s.as_table())
    {
        for (key, item) in state.iter() {
            if let Some(hash) = item
                .as_table()
                .and_then(|t| t.get("trusted_hash"))
                .and_then(|v| v.as_str())
            {
                map.insert(key.to_string(), hash.to_string());
            }
        }
    }
    map
}

/// 写入/更新 config.toml 的 `[hooks.state.<key>] trusted_hash`（toml_edit 保留格式最小化编辑）。
fn write_codex_trust(entries: &[(String, String)]) -> Result<()> {
    use toml_edit::{DocumentMut, Item, Table};
    let path = paths::codex_config_toml();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| anyhow!("解析 config.toml 失败，已中止（不覆盖原文件）：{e}"))?;

    fn ensure_intermediate<'a>(parent: &'a mut Table, key: &str) -> Result<&'a mut Table> {
        if !parent.contains_key(key) {
            let mut t = Table::new();
            t.set_implicit(true);
            parent.insert(key, Item::Table(t));
        }
        parent
            .get_mut(key)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| anyhow!("config.toml 中 `{key}` 不是表，已中止"))
    }

    let hooks = ensure_intermediate(doc.as_table_mut(), "hooks")?;
    let state = ensure_intermediate(hooks, "state")?;
    for (key, hash) in entries {
        if !state.contains_key(key) {
            state.insert(key, Item::Table(Table::new()));
        }
        let entry = state
            .get_mut(key)
            .and_then(Item::as_table_mut)
            .ok_or_else(|| anyhow!("config.toml 中信任条目不是表，已中止"))?;
        entry.insert("trusted_hash", toml_edit::value(hash.clone()));
    }
    write_toml(&path, &doc.to_string())
}

/// 移除 config.toml 中所有以 `<abs hooks.json>:` 为前缀的信任条目；空表则删除。
fn remove_codex_trust_for(hooks_json: &std::path::Path) -> Result<()> {
    use toml_edit::{DocumentMut, Item};
    let path = paths::codex_config_toml();
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let mut doc = existing
        .parse::<DocumentMut>()
        .map_err(|e| anyhow!("解析 config.toml 失败，已中止（不覆盖原文件）：{e}"))?;
    let prefix = format!("{}:", hooks_json.to_string_lossy());

    let mut changed = false;
    if let Some(hooks) = doc.get_mut("hooks").and_then(Item::as_table_mut) {
        if let Some(state) = hooks.get_mut("state").and_then(Item::as_table_mut) {
            let before = state.len();
            state.retain(|k, _| !k.starts_with(&prefix));
            changed = state.len() != before;
            if state.is_empty() {
                hooks.remove("state");
            }
        }
        if hooks.is_empty() {
            doc.as_table_mut().remove("hooks");
        }
    }
    if changed {
        write_toml(&path, &doc.to_string())?;
    }
    Ok(())
}

// ===== 私有 IO =====

fn write_text(path: &std::path::Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write(path, text.as_bytes())
}

fn write_toml(path: &std::path::Path, text: &str) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    atomic_write(path, text.as_bytes())
}

fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/opt/AskHuman";

    fn to_value(text: &str) -> Value {
        jsonc_parser::parse_to_serde_value(text, &ParseOptions::default()).unwrap()
    }

    #[test]
    fn claude_install_adds_all_events_nested() {
        let out = apply_json_install(AgentKind::Claude, EXE, "{}", Shape::Nested).unwrap();
        let v = to_value(&out);
        for (ev, lc) in [
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "turn-start"),
            ("PreToolUse", "activity"),
            ("PostToolUse", "activity"),
            ("Stop", "turn-end"),
            ("StopFailure", "turn-end"),
            ("SessionEnd", "session-end"),
        ] {
            let arr = v["hooks"][ev].as_array().unwrap();
            assert_eq!(arr.len(), 1, "event {ev} should have one entry");
            let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
            if ev == "Stop" {
                assert!(cmd.contains(super::super::agent_stop::MARKER));
                assert!(cmd.contains("claude") && cmd.contains("track"));
            } else {
                assert!(cmd.contains(MARKER) && cmd.contains("claude") && cmd.contains(lc));
            }
        }
    }

    #[test]
    fn long_running_hooks_get_explicit_timeout() {
        // Interjection waits and Stop confirmation need a long timeout; other hooks use defaults.
        // Claude / Codex（Nested）。
        for kind in [AgentKind::Claude, AgentKind::Codex] {
            let out = apply_json_install(kind, EXE, "{}", Shape::Nested).unwrap();
            let v = to_value(&out);
            assert_eq!(
                v["hooks"]["PreToolUse"][0]["hooks"][0]["timeout"].as_u64(),
                Some(86400),
                "{kind:?} PreToolUse 应带 timeout"
            );
            assert!(
                v["hooks"]["PostToolUse"][0]["hooks"][0]
                    .get("timeout")
                    .is_none(),
                "{kind:?} 其余事件不写 timeout"
            );
            assert_eq!(
                v["hooks"]["Stop"][0]["hooks"][0]["timeout"].as_u64(),
                Some(86400)
            );
        }
        // Cursor（Flat）。
        let out = apply_json_install(AgentKind::Cursor, EXE, "{}", Shape::Flat).unwrap();
        let v = to_value(&out);
        assert_eq!(v["hooks"]["preToolUse"][0]["timeout"].as_u64(), Some(86400));
        assert!(v["hooks"]["postToolUse"][0].get("timeout").is_none());
        assert_eq!(v["hooks"]["stop"][0]["timeout"].as_u64(), Some(86400));
        assert!(v["hooks"]["stop"][0]["loop_limit"].is_null());
        // Grok 首期排除：任何事件都不写 timeout。
        let out = apply_json_install(AgentKind::Grok, EXE, "{}", Shape::Nested).unwrap();
        let v = to_value(&out);
        assert!(v["hooks"]["PreToolUse"][0]["hooks"][0]
            .get("timeout")
            .is_none());
    }

    #[test]
    fn old_install_without_timeout_is_incomplete() {
        // An older artifact without the explicit PreToolUse timeout must be surfaced as outdated;
        // the Agent integration update flow repairs it only after the user chooses Update.
        // 用「新版安装产物手动抹掉 timeout」模拟旧产物。
        let new = apply_json_install(AgentKind::Claude, EXE, "{}", Shape::Nested).unwrap();
        let mut old = to_value(&new);
        old["hooks"]["PreToolUse"][0]["hooks"][0]
            .as_object_mut()
            .unwrap()
            .remove("timeout");
        let (any, complete) = json_presence(AgentKind::Claude, EXE, &old, Shape::Nested);
        assert!(any, "旧产物仍算已安装");
        assert!(!complete, "缺 timeout 应判不完整（outdated）");
        // 重装（幂等替换）后完整。
        let migrated =
            apply_json_install(AgentKind::Claude, EXE, &old.to_string(), Shape::Nested).unwrap();
        let (any, complete) =
            json_presence(AgentKind::Claude, EXE, &to_value(&migrated), Shape::Nested);
        assert!(any && complete);
        // Cursor Flat 同理。
        let new = apply_json_install(AgentKind::Cursor, EXE, "{}", Shape::Flat).unwrap();
        let mut old = to_value(&new);
        old["hooks"]["preToolUse"][0]
            .as_object_mut()
            .unwrap()
            .remove("timeout");
        let (any, complete) = json_presence(AgentKind::Cursor, EXE, &old, Shape::Flat);
        assert!(any && !complete);
    }

    #[test]
    fn fresh_install_is_complete() {
        for (kind, shape) in [
            (AgentKind::Claude, Shape::Nested),
            (AgentKind::Codex, Shape::Nested),
            (AgentKind::Cursor, Shape::Flat),
            (AgentKind::Grok, Shape::Nested),
        ] {
            let out = apply_json_install(kind, EXE, "{}", shape).unwrap();
            let (any, complete) = json_presence(kind, EXE, &to_value(&out), shape);
            assert!(any && complete, "{kind:?} 新装即完整");
        }
    }

    #[test]
    fn lifecycle_stop_confirmation_variant_is_complete_and_path_sensitive() {
        for (kind, shape) in [
            (AgentKind::Claude, Shape::Nested),
            (AgentKind::Codex, Shape::Nested),
            (AgentKind::Cursor, Shape::Flat),
        ] {
            let output = apply_json_install_with_stop(kind, EXE, "{}", shape, true).unwrap();
            let value = to_value(&output);
            let (any, complete) = json_presence_with_stop(kind, EXE, &value, shape, true);
            assert!(any && complete);
            let (_, without_confirm) = json_presence(kind, EXE, &value, shape);
            assert!(!without_confirm);
            let (_, old_binary) =
                json_presence_with_stop(kind, "/old/AskHuman", &value, shape, true);
            assert!(!old_binary);
            let event = if kind == AgentKind::Cursor {
                "stop"
            } else {
                "Stop"
            };
            let handler = if shape == Shape::Flat {
                &value["hooks"][event][0]
            } else {
                &value["hooks"][event][0]["hooks"][0]
            };
            let command = handler["command"].as_str().unwrap();
            assert!(command.ends_with(&format!("{} track confirm", kind.as_str())));
        }
    }

    #[test]
    fn grok_install_adds_all_events_nested() {
        // Grok 与 Claude 同构（Nested、PascalCase、事件最全），装 grok 原生 hook 命令含 `grok`。
        let out = apply_json_install(AgentKind::Grok, EXE, "{}", Shape::Nested).unwrap();
        let v = to_value(&out);
        for (ev, lc) in [
            ("SessionStart", "session-start"),
            ("UserPromptSubmit", "turn-start"),
            ("PreToolUse", "activity"),
            ("PostToolUse", "activity"),
            ("Stop", "turn-end"),
            ("StopFailure", "turn-end"),
            ("SessionEnd", "session-end"),
        ] {
            let arr = v["hooks"][ev].as_array().unwrap();
            assert_eq!(arr.len(), 1, "event {ev} should have one entry");
            let cmd = arr[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.contains(MARKER) && cmd.contains("grok") && cmd.contains(lc));
        }
    }

    #[test]
    fn cursor_install_flat_with_version() {
        let out = apply_json_install(AgentKind::Cursor, EXE, "{}", Shape::Flat).unwrap();
        let v = to_value(&out);
        assert_eq!(v["version"], 1);
        let arr = v["hooks"]["beforeSubmitPrompt"].as_array().unwrap();
        assert!(arr[0]["command"].as_str().unwrap().contains(MARKER));
        assert!(arr[0].get("hooks").is_none(), "flat 形状无嵌套 hooks");
    }

    #[test]
    fn install_is_idempotent_fixpoint() {
        let a = apply_json_install(AgentKind::Claude, EXE, "{}", Shape::Nested).unwrap();
        let b = apply_json_install(AgentKind::Claude, EXE, &a, Shape::Nested).unwrap();
        let c = apply_json_install(AgentKind::Claude, EXE, &b, Shape::Nested).unwrap();
        assert_eq!(b, c);
        let v = to_value(&c);
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn install_preserves_existing_timeout_hook() {
        // 既有 timeout hook（PreToolUse/Bash + askhuman-timeout.sh）应原样保留。
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"x/askhuman-timeout.sh\" } ] } ] } }";
        let out = apply_json_install(AgentKind::Claude, EXE, input, Shape::Nested).unwrap();
        let v = to_value(&out);
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "x/askhuman-timeout.sh"
        );
        assert!(v["hooks"]["SessionStart"].as_array().unwrap().len() == 1);
    }

    #[test]
    fn uninstall_removes_only_ours() {
        let input = "{ \"hooks\": { \"PreToolUse\": [ { \"matcher\": \"Bash\", \"hooks\": [ { \"type\": \"command\", \"command\": \"x/askhuman-timeout.sh\" } ] } ], \"SessionStart\": [ { \"hooks\": [ { \"type\": \"command\", \"command\": \"a __agent-hook claude session-start\" } ] } ] } }";
        let out = apply_json_uninstall(AgentKind::Claude, input, Shape::Nested).unwrap();
        let v = to_value(&out);
        assert!(v["hooks"].get("SessionStart").is_none(), "空数组应删键");
        assert_eq!(
            v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "x/askhuman-timeout.sh",
            "timeout hook 应保留"
        );
    }

    #[test]
    fn parse_error_aborts() {
        assert!(
            apply_json_install(AgentKind::Claude, EXE, "{ \"hooks\": ", Shape::Nested).is_err()
        );
        assert!(apply_json_uninstall(AgentKind::Claude, "{ \"hooks\": ", Shape::Nested).is_err());
    }

    #[test]
    fn codex_trusted_hash_matches_reference_algorithm() {
        // 与 codex-trust.cjs 同输入应得同输出（键排序紧凑 JSON 的 sha256）。
        let cmd = "\"/opt/AskHuman\" __agent-hook codex session-start";
        let h = codex_trusted_hash("session_start", cmd, 600);
        // 独立计算参考值。
        let serialized = format!(
            "{{\"event_name\":\"session_start\",\"hooks\":[{{\"async\":false,\"command\":{},\"timeout\":600,\"type\":\"command\"}}]}}",
            serde_json::to_string(cmd).unwrap()
        );
        let mut hasher = Sha256::new();
        hasher.update(serialized.as_bytes());
        let want = format!("sha256:{}", hex_encode(&hasher.finalize()));
        assert_eq!(h, want);
    }

    #[test]
    fn codex_trusted_hash_includes_timeout() {
        // PreToolUse 写 timeout=86400 → 信任哈希随 timeout 变化（旧哈希按 600 算 → 自动判过期）。
        let cmd = "\"/opt/AskHuman\" __agent-hook codex activity";
        let h = codex_trusted_hash("pre_tool_use", cmd, 86400);
        let serialized = format!(
            "{{\"event_name\":\"pre_tool_use\",\"hooks\":[{{\"async\":false,\"command\":{},\"timeout\":86400,\"type\":\"command\"}}]}}",
            serde_json::to_string(cmd).unwrap()
        );
        let mut hasher = Sha256::new();
        hasher.update(serialized.as_bytes());
        let want = format!("sha256:{}", hex_encode(&hasher.finalize()));
        assert_eq!(h, want);
        assert_ne!(h, codex_trusted_hash("pre_tool_use", cmd, 600));
    }

    #[test]
    fn codex_trust_entries_include_shared_stop_handler() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hooks.json");
        let command = "\"/opt/AskHuman\" __stop-hook codex track confirm";
        std::fs::write(
            &path,
            serde_json::json!({
                "hooks": {
                    "Stop": [{
                        "hooks": [{
                            "type": "command",
                            "command": command,
                            "timeout": 86400
                        }]
                    }]
                }
            })
            .to_string(),
        )
        .unwrap();
        let entries = codex_trust_entries(&path).unwrap();
        assert_eq!(entries.len(), 1);
        assert!(entries[0].0.ends_with(":stop:0:0"));
        assert_eq!(entries[0].1, codex_trusted_hash("stop", command, 86400));
    }

    #[test]
    fn canonical_compact_sorts_keys() {
        let v = serde_json::json!({ "b": 1, "a": [ { "y": 2, "x": 1 } ] });
        let mut s = String::new();
        canonical_compact(&v, &mut s);
        assert_eq!(s, "{\"a\":[{\"x\":1,\"y\":2}],\"b\":1}");
    }

    #[test]
    fn lifecycle_status_matrix_binds_artifacts_to_active_modes() {
        let missing = DiskLifecycleStatus {
            installed: false,
            outdated: false,
            supported: true,
        };
        let installed = DiskLifecycleStatus {
            installed: true,
            ..missing
        };
        let outdated = DiskLifecycleStatus {
            outdated: true,
            ..installed
        };

        let none_clean =
            aggregate_status(crate::integrations::agent_mode::Mode::None, None, missing);
        assert!(!none_clean.enabled && !none_clean.needs_update);

        let none_legacy = aggregate_status(
            crate::integrations::agent_mode::Mode::None,
            Some(true),
            installed,
        );
        assert!(!none_legacy.enabled);
        assert!(none_legacy.needs_update && none_legacy.cleanup_required);

        let legacy_active =
            aggregate_status(crate::integrations::agent_mode::Mode::Cli, None, missing);
        assert!(legacy_active.enabled && !legacy_active.preference_configured);
        assert!(legacy_active.needs_update && !legacy_active.cleanup_required);

        let explicit_off = aggregate_status(
            crate::integrations::agent_mode::Mode::Mcp,
            Some(false),
            missing,
        );
        assert!(!explicit_off.enabled && !explicit_off.needs_update);

        let off_drift = aggregate_status(
            crate::integrations::agent_mode::Mode::Mcp,
            Some(false),
            installed,
        );
        assert!(off_drift.needs_update && off_drift.cleanup_required);

        let on_outdated = aggregate_status(
            crate::integrations::agent_mode::Mode::Cli,
            Some(true),
            outdated,
        );
        assert!(on_outdated.enabled && on_outdated.needs_update && on_outdated.outdated);
    }

    #[test]
    fn lifecycle_preferences_cover_every_agent_kind() {
        let mut preferences = LifecyclePreferences::default();
        for kind in [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Cursor,
            AgentKind::Grok,
            AgentKind::Pi,
        ] {
            assert_eq!(preferences.get(kind), None);
            preferences.set(kind, true);
            assert_eq!(preferences.get(kind), Some(true));
            preferences.set(kind, false);
            assert_eq!(preferences.get(kind), Some(false));
        }
    }

    #[test]
    fn lifecycle_preferences_are_backward_compatible_and_fail_closed_to_defaults() {
        let missing_fields = parse_preferences(br#"{"codex":false,"pi":true}"#);
        assert_eq!(missing_fields.get(AgentKind::Claude), None);
        assert_eq!(missing_fields.get(AgentKind::Codex), Some(false));
        assert_eq!(missing_fields.get(AgentKind::Pi), Some(true));

        let empty = parse_preferences(b"{}");
        assert_eq!(empty.get(AgentKind::Cursor), None);

        let corrupt = parse_preferences(b"not json");
        for kind in [
            AgentKind::Claude,
            AgentKind::Codex,
            AgentKind::Cursor,
            AgentKind::Grok,
            AgentKind::Pi,
        ] {
            assert_eq!(corrupt.get(kind), None);
        }
    }
}
