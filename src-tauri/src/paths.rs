//! 跨平台路径：配置目录、临时目录、Cursor 相关路径。

use std::path::PathBuf;

/// 用户主目录（解析失败时回退到当前目录，保证不 panic）。
pub fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

/// 配置目录：默认 `~/.askhuman`；若设置了非空 `ASKHUMAN_HOME` 则用该路径
/// （Dev Instance / 测试隔离，见 `dev_instance` 与 `docs/specs/dev-instance-parallel.md`）。
pub fn config_dir() -> PathBuf {
    if let Ok(raw) = std::env::var(crate::dev_instance::ASKHUMAN_HOME_ENV) {
        if !raw.is_empty() {
            let p = PathBuf::from(raw);
            if p.is_absolute() {
                return p;
            }
            return std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p);
        }
    }
    home().join(".askhuman")
}

/// 旧版配置目录 `~/.humaninloop`（仅用于向后兼容读取；Dev Instance 模式不回退到此）。
pub fn legacy_config_dir() -> PathBuf {
    home().join(".humaninloop")
}

/// 机器级 dev 渠道预设目录 `~/.askhuman/dev-presets/`（固定在用户主 ashuman 树下，
/// **不**随 `ASKHUMAN_HOME` 变；主 daemon 不加载。P1b 使用）。
pub fn dev_presets_dir() -> PathBuf {
    home().join(".askhuman").join("dev-presets")
}

/// 配置文件 `~/.askhuman/config.json`。
pub fn config_file() -> PathBuf {
    config_dir().join("config.json")
}

/// 旧版配置文件 `~/.humaninloop/config.json`（仅用于向后兼容读取）。
pub fn legacy_config_file() -> PathBuf {
    legacy_config_dir().join("config.json")
}

/// 本次请求的图片落盘目录 `temp/askhuman/<request_id>/`。
pub fn request_temp_dir(request_id: &str) -> PathBuf {
    std::env::temp_dir().join("askhuman").join(request_id)
}

/// Managed original files and thumbnail caches owned by project todos.
pub fn todo_attachments_dir() -> PathBuf {
    state_dir().join("todo-attachments")
}

/// Managed files for one todo. Callers must validate `todo_id` as a UUID first.
pub fn todo_attachment_dir(todo_id: &str) -> PathBuf {
    todo_attachments_dir().join(todo_id)
}

/// Request-scoped copies delivered to an Agent. The existing daemon temp GC removes the parent.
pub fn todo_delivery_dir(request_id: &str) -> PathBuf {
    request_temp_dir(request_id).join("todo-files")
}

/// 回复历史文件 `~/.askhuman/history.jsonl`（每行一条 JSON）。
pub fn history_file() -> PathBuf {
    config_dir().join("history.jsonl")
}

/// 回复历史写入锁 `~/.askhuman/history.lock`（写/裁剪/清空时持有）。
pub fn history_lock() -> PathBuf {
    config_dir().join("history.lock")
}

/// Private overwrite-only files used when a recovered AskHuman Message exceeds stdout limits.
pub fn show_last_dir() -> PathBuf {
    state_dir().join("show-last")
}

/// Private schema-hidden MCP session tokens, consumed once by AskHuman MCP handlers.
pub fn session_token_dir() -> PathBuf {
    state_dir().join("session-tokens")
}

/// 版本自更新状态文件 `~/.askhuman/update.json`（最新版本/检查时间/忽略集合/待生效）。
pub fn update_state_file() -> PathBuf {
    config_dir().join("update.json")
}

/// Cross-process write lock for `update.json`.
pub fn update_state_lock() -> PathBuf {
    config_dir().join("update.lock")
}

/// 自动集成产物的内部所有权账本 `~/.askhuman/integration-state.json`。
///
/// 它与用户设置 `config.json` 分离，只用于区分“用户预存配置”和“AskHuman 实际追加配置”，
/// 从而在卸载时只删除本应用拥有的增量。
pub fn integration_state_file() -> PathBuf {
    config_dir().join("integration-state.json")
}

/// Cross-process lock for all integration artifact mutations.
pub fn integrations_lock_file() -> PathBuf {
    config_dir().join("integrations.lock")
}

/// Per-agent PermissionRequest preference; independent from the integration mode.
pub fn permission_preferences_file() -> PathBuf {
    config_dir().join("permission-preferences.json")
}

/// Per-agent Stop confirmation preference; independent from lifecycle tracking and integration mode.
pub fn stop_preferences_file() -> PathBuf {
    config_dir().join("stop-preferences.json")
}

/// Per-agent lifecycle tracking preference. Tracking is available only while the corresponding
/// automatic Agent integration is active, but the explicit preference survives mode changes.
pub fn lifecycle_preferences_file() -> PathBuf {
    config_dir().join("lifecycle-preferences.json")
}

/// Claude 提问接管的开关（spec claude-ask-user-question D3），与 permission / stop 偏好同级。
pub fn ask_question_preferences_file() -> PathBuf {
    config_dir().join("ask-question-preferences.json")
}

/// Agent 生命周期追踪状态文件 `~/.askhuman/agents.json`（daemon 持久化、重启复核用）。
pub fn agents_file() -> PathBuf {
    config_dir().join("agents.json")
}

/// 杂项运行时状态目录 `~/.askhuman/state`（daemon 跨重启保留的小状态）。
pub fn state_dir() -> PathBuf {
    config_dir().join("state")
}

/// 「IM 会话期自动激活」当前活跃槽持久化文件 `~/.askhuman/state/auto-channel.json`。
/// 跨 daemon 重启保留；仅由「用户在某渠道的入站消息」更新。
pub fn auto_channel_file() -> PathBuf {
    state_dir().join("auto-channel.json")
}

/// `/watch` 实时关注订阅持久化文件 `~/.askhuman/state/watch.json`（跨 daemon 重启恢复，
/// 恢复后继续编辑同一张卡，见 `docs/specs/im-watch.md`）。
pub fn watch_file() -> PathBuf {
    state_dir().join("watch.json")
}

/// Agent 插话待送达队列持久化文件 `~/.askhuman/state/interject.json`（跨 daemon 重启恢复；
/// 只在变更时写、启动读一次——hook 热路径零文件 IO，见 `docs/specs/agent-interject.md` D8）。
pub fn interject_file() -> PathBuf {
    state_dir().join("interject.json")
}

/// `/msg` 一次性输入卡的最小恢复账本。只保存卡片定位与目标会话，不保存用户输入内容。
pub fn msg_compose_file() -> PathBuf {
    state_dir().join("msg-compose.json")
}

/// Workspaces discovered from recent local Agent sessions and user curation.
pub fn agent_workspaces_file() -> PathBuf {
    config_dir().join("agent-workspaces.json")
}

/// Cross-process lock for workspace discovery and curation.
pub fn agent_workspaces_lock() -> PathBuf {
    config_dir().join("agent-workspaces.lock")
}

/// Private, one-time launch records consumed by `AskHuman __agent-launch`.
pub fn agent_launch_dir() -> PathBuf {
    state_dir().join("agent-launches")
}

/// GUI 宿主进程的 IPC socket `~/.askhuman/gui-host.sock`（与 daemon socket 解耦，
/// 使 daemon 未运行时也能打开设置/历史窗口，spec D13）。
#[cfg(unix)]
pub fn gui_host_sock() -> PathBuf {
    config_dir().join("gui-host.sock")
}

/// GUI 宿主进程的跨平台单实例锁 `~/.askhuman/gui-host.lock`。
pub fn gui_host_lock() -> PathBuf {
    config_dir().join("gui-host.lock")
}

/// Cursor 目录 `~/.cursor`。
pub fn cursor_dir() -> PathBuf {
    home().join(".cursor")
}

/// `~/.cursor/hooks.json`。
pub fn cursor_hooks_json() -> PathBuf {
    cursor_dir().join("hooks.json")
}

/// AskHuman-owned Cursor timeout hook (`.sh` on Unix, `.ps1` on Windows).
pub fn cursor_hook_script() -> PathBuf {
    cursor_dir().join("hooks").join(if cfg!(windows) {
        "askhuman-timeout.ps1"
    } else {
        "askhuman-timeout.sh"
    })
}

/// 旧版 hook 脚本 `~/.cursor/hooks/humaninloop-timeout.sh`（仅用于向后兼容清理）。
pub fn legacy_cursor_hook_script() -> PathBuf {
    cursor_dir().join("hooks").join("humaninloop-timeout.sh")
}

/// Cursor 全局规则目录 `~/.cursor/rules`（用户级，跨项目，文件为 `*.mdc`）。
pub fn cursor_rules_dir() -> PathBuf {
    cursor_dir().join("rules")
}

/// 本应用独占的 Cursor 全局规则文件 `~/.cursor/rules/askhuman.mdc`。
pub fn cursor_rule_file() -> PathBuf {
    cursor_rules_dir().join("askhuman.mdc")
}

/// Cursor 用户级 MCP 配置文件 `~/.cursor/mcp.json`（与 hooks.json 不同文件）。
pub fn cursor_mcp_json() -> PathBuf {
    cursor_dir().join("mcp.json")
}

/// Claude Code 配置目录 `~/.claude`。
pub fn claude_dir() -> PathBuf {
    home().join(".claude")
}

/// Claude Code 全局 memory 文件 `~/.claude/CLAUDE.md`（用户级，跨项目）。
pub fn claude_md() -> PathBuf {
    claude_dir().join("CLAUDE.md")
}

/// Claude Code 用户级设置文件 `~/.claude/settings.json`（hooks / env 等都写在此）。
pub fn claude_settings_json() -> PathBuf {
    claude_dir().join("settings.json")
}

/// Claude Code 用户级主配置文件 `~/.claude.json`（top-level `mcpServers` 写于此；
/// 文件通常很大、含大量项目历史，必须最小化编辑、绝不整写）。
pub fn claude_json() -> PathBuf {
    home().join(".claude.json")
}

/// AskHuman-owned Claude Code timeout hook (`.sh` on Unix, `.ps1` on Windows).
pub fn claude_hook_script() -> PathBuf {
    claude_dir().join("hooks").join(if cfg!(windows) {
        "askhuman-timeout.ps1"
    } else {
        "askhuman-timeout.sh"
    })
}

/// Codex 配置目录 `~/.codex`。
pub fn codex_dir() -> PathBuf {
    home().join(".codex")
}

/// Codex 全局指令文件 `~/.codex/AGENTS.md`（用户级，跨项目）。
pub fn codex_agents_md() -> PathBuf {
    codex_dir().join("AGENTS.md")
}

/// Codex 用户级配置文件 `~/.codex/config.toml`（生命周期 hook 的信任 `[hooks.state]` 写于此）。
pub fn codex_config_toml() -> PathBuf {
    codex_dir().join("config.toml")
}

/// Codex 用户级 hook 定义文件 `~/.codex/hooks.json`（生命周期 hook 写于此；信任键以其绝对路径为前缀）。
pub fn codex_hooks_json() -> PathBuf {
    codex_dir().join("hooks.json")
}

/// Grok 配置目录 `~/.grok`。
pub fn grok_dir() -> PathBuf {
    home().join(".grok")
}

/// Grok 用户级配置文件 `~/.grok/config.toml`（`[mcp_servers.askhuman]` 写于此）。
pub fn grok_config_toml() -> PathBuf {
    grok_dir().join("config.toml")
}

/// 本应用独占的 Grok skill 目录 `~/.grok/skills/interaction-protocol`。
pub fn grok_skill_dir() -> PathBuf {
    grok_dir().join("skills").join("interaction-protocol")
}

/// 本应用独占的 Grok skill 文件 `~/.grok/skills/interaction-protocol/SKILL.md`（必读交互协议载体）。
pub fn grok_skill_md() -> PathBuf {
    grok_skill_dir().join("SKILL.md")
}

/// Grok 用户级 hook 目录 `~/.grok/hooks`（全局 hook 恒受信任，无需 trust 哈希）。
pub fn grok_hooks_dir() -> PathBuf {
    grok_dir().join("hooks")
}

/// 本应用独占的 Grok 生命周期 hook 定义文件 `~/.grok/hooks/askhuman-lifecycle.json`。
pub fn grok_hooks_json() -> PathBuf {
    grok_hooks_dir().join("askhuman-lifecycle.json")
}

/// Grok 会话目录 `~/.grok/sessions`（子目录为 URL 编码的 cwd，再下一层为 `<session_id>/`）。
pub fn grok_sessions_dir() -> PathBuf {
    grok_dir().join("sessions")
}

/// Pi coding-agent user directory `~/.pi/agent`.
pub fn pi_agent_dir() -> PathBuf {
    home().join(".pi").join("agent")
}

/// Pi global instructions file `~/.pi/agent/AGENTS.md`.
pub fn pi_agents_md() -> PathBuf {
    pi_agent_dir().join("AGENTS.md")
}

/// AskHuman-owned Pi extension directory.
pub fn pi_extension_dir() -> PathBuf {
    pi_agent_dir().join("extensions").join("askhuman")
}

/// AskHuman-owned Pi extension entry point.
pub fn pi_extension_file() -> PathBuf {
    pi_extension_dir().join("index.ts")
}

/// Pi's default session root. Custom session directories are reported by the extension at runtime.
pub fn pi_sessions_dir() -> PathBuf {
    pi_agent_dir().join("sessions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // env mutations must not run concurrently with other tests that touch ASKHUMAN_HOME.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn config_dir_respects_askhuman_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let prev = std::env::var_os(crate::dev_instance::ASKHUMAN_HOME_ENV);
        let custom = std::env::temp_dir().join("askhuman-home-test-xyz");
        std::env::set_var(crate::dev_instance::ASKHUMAN_HOME_ENV, &custom);
        assert_eq!(config_dir(), custom);
        match prev {
            Some(v) => std::env::set_var(crate::dev_instance::ASKHUMAN_HOME_ENV, v),
            None => std::env::remove_var(crate::dev_instance::ASKHUMAN_HOME_ENV),
        }
    }

    #[test]
    fn dev_presets_dir_not_under_askhuman_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let prev = std::env::var_os(crate::dev_instance::ASKHUMAN_HOME_ENV);
        std::env::set_var(
            crate::dev_instance::ASKHUMAN_HOME_ENV,
            std::env::temp_dir().join("instance-home-only"),
        );
        assert_eq!(
            dev_presets_dir(),
            home().join(".askhuman").join("dev-presets")
        );
        assert_ne!(dev_presets_dir(), config_dir().join("dev-presets"));
        match prev {
            Some(v) => std::env::set_var(crate::dev_instance::ASKHUMAN_HOME_ENV, v),
            None => std::env::remove_var(crate::dev_instance::ASKHUMAN_HOME_ENV),
        }
    }
}
