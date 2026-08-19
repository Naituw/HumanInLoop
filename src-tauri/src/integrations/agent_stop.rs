//! Per-agent Stop confirmation capability. The product preference is preserved independently,
//! but confirmation is active only while the agent integration mode is CLI or MCP. Lifecycle is
//! an optional capability inside that same integration, and both capabilities share exactly one
//! AskHuman-owned Stop handler on disk.

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::AgentKind;

use super::hook_edit;

pub const MARKER: &str = "__stop-hook";
pub const TIMEOUT_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopStatus {
    pub supported: bool,
    pub enabled: bool,
    pub installed: bool,
    pub outdated: bool,
    pub other_handlers_detected: bool,
}

struct HandlerState {
    configured: bool,
    outdated: bool,
    other_handlers_detected: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Preferences {
    #[serde(default)]
    claude: Option<bool>,
    #[serde(default)]
    codex: Option<bool>,
    #[serde(default)]
    cursor: Option<bool>,
    #[serde(default)]
    pi: Option<bool>,
}

pub fn supported(kind: AgentKind) -> bool {
    kind != AgentKind::Grok
}

pub fn enabled(kind: AgentKind) -> bool {
    if !supported(kind) {
        return false;
    }
    let preferences = load_preferences();
    match kind {
        AgentKind::Claude => preferences.claude.unwrap_or(false),
        AgentKind::Codex => preferences.codex.unwrap_or(false),
        AgentKind::Cursor => preferences.cursor.unwrap_or(false),
        AgentKind::Grok => false,
        AgentKind::Pi => preferences.pi.unwrap_or(true),
    }
}

pub fn set_enabled(kind: AgentKind, value: bool) -> Result<()> {
    if !supported(kind) {
        return Err(anyhow!("Stop confirmation is unsupported for this agent"));
    }
    let _lock = super::mutation_lock::IntegrationMutationLock::acquire()?;
    let original = load_preferences();
    let mut preferences = original.clone();
    match kind {
        AgentKind::Claude => preferences.claude = Some(value),
        AgentKind::Codex => preferences.codex = Some(value),
        AgentKind::Cursor => preferences.cursor = Some(value),
        AgentKind::Grok => unreachable!(),
        AgentKind::Pi => preferences.pi = Some(value),
    }
    save_preferences(&preferences)?;
    if let Err(error) = reconcile_current_mode_unlocked(kind) {
        let _ = save_preferences(&original);
        return Err(error);
    }
    Ok(())
}

fn confirmation_active(preference_enabled: bool, mode: super::agent_mode::Mode) -> bool {
    preference_enabled && mode != super::agent_mode::Mode::None
}

/// Whether Stop confirmation should be installed for the requested integration mode.
pub(crate) fn active_in_mode(kind: AgentKind, mode: super::agent_mode::Mode) -> bool {
    confirmation_active(enabled(kind), mode)
}

/// Whether Stop confirmation should be installed for the mode currently represented on disk.
pub(crate) fn active_in_current_mode(kind: AgentKind) -> bool {
    active_in_mode(kind, super::agent_mode::current(target_for_kind(kind)))
}

pub fn status(kind: AgentKind) -> StopStatus {
    if !supported(kind) {
        return StopStatus {
            supported: false,
            enabled: false,
            installed: false,
            outdated: false,
            other_handlers_detected: false,
        };
    }
    if kind == AgentKind::Pi {
        let preference_enabled = enabled(kind);
        let active = confirmation_active(
            preference_enabled,
            super::agent_mode::current(target_for_kind(kind)),
        );
        let extension = super::pi_extension::status();
        return StopStatus {
            supported: true,
            enabled: preference_enabled,
            installed: extension.stop,
            outdated: extension.stop != active || (active && extension.outdated),
            other_handlers_detected: false,
        };
    }
    let track = super::agent_lifecycle::tracking_installed(kind);
    let preference_enabled = enabled(kind);
    let confirm = confirmation_active(
        preference_enabled,
        super::agent_mode::current(target_for_kind(kind)),
    );
    let expected = hook_command(kind, track, confirm).unwrap_or_default();
    let expected_windows = (kind == AgentKind::Codex)
        .then(|| {
            std::env::current_exe().map(|executable| {
                windows_hook_command_for(&executable.to_string_lossy(), kind, track, confirm)
            })
        })
        .transpose()
        .unwrap_or_default();
    let path = hook_path(kind);
    let text = std::fs::read_to_string(path).unwrap_or_else(|_| "{}".into());
    let handler_state = inspect_handler_state(
        kind,
        &text,
        &expected,
        expected_windows.as_deref(),
        track || confirm,
    );
    StopStatus {
        supported: true,
        enabled: preference_enabled,
        installed: handler_state.configured,
        outdated: handler_state.outdated,
        other_handlers_detected: handler_state.other_handlers_detected,
    }
}

fn inspect_handler_state(
    kind: AgentKind,
    text: &str,
    expected: &str,
    expected_windows: Option<&str>,
    desired_exists: bool,
) -> HandlerState {
    let handlers = stop_handlers(kind, text);
    let mut marker_count = 0usize;
    let mut exact_count = 0usize;
    let mut configured = false;
    let mut other_handlers_detected = false;
    for handler in handlers {
        let command = handler.get("command").and_then(Value::as_str).unwrap_or("");
        if command.contains(MARKER) {
            marker_count += 1;
            configured |= command.split_whitespace().any(|part| part == "confirm");
            let timeout_ok = handler.get("timeout").and_then(Value::as_u64) == Some(TIMEOUT_SECS);
            let loop_ok =
                kind != AgentKind::Cursor || handler.get("loop_limit").is_some_and(Value::is_null);
            if hook_edit::command_handler_matches(&handler, expected, expected_windows)
                && timeout_ok
                && loop_ok
            {
                exact_count += 1;
            }
        } else {
            other_handlers_detected = true;
        }
    }
    HandlerState {
        configured,
        outdated: if desired_exists {
            marker_count != 1 || exact_count != 1
        } else {
            marker_count != 0
        },
        other_handlers_detected,
    }
}

/// Reconcile after an integration mode change. Caller holds the integration mutation lock.
pub(crate) fn reconcile_unlocked(kind: AgentKind, mode: super::agent_mode::Mode) -> Result<()> {
    if !supported(kind) {
        return Ok(());
    }
    if kind == AgentKind::Pi {
        return super::pi_extension::set_stop_enabled(active_in_mode(kind, mode));
    }
    let track = super::agent_lifecycle::tracking_installed(kind);
    let confirm = active_in_mode(kind, mode);
    if track || confirm {
        install_handler(kind, track, confirm)
    } else {
        remove_handler(kind)
    }
}

/// Reconcile after either preference or lifecycle tracking changes. Caller holds the integration
/// mutation lock, and the current mode is resolved from the other on-disk integration artifacts.
pub(crate) fn reconcile_current_mode_unlocked(kind: AgentKind) -> Result<()> {
    reconcile_unlocked(kind, super::agent_mode::current(target_for_kind(kind)))
}

pub fn migrate_outdated() -> Vec<AgentKind> {
    let mut migrated = Vec::new();
    for kind in [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Cursor,
        AgentKind::Pi,
    ] {
        let state = status(kind);
        if state.outdated {
            if let Ok(_lock) = super::mutation_lock::IntegrationMutationLock::acquire() {
                if reconcile_current_mode_unlocked(kind).is_ok() {
                    migrated.push(kind);
                }
            }
        }
    }
    migrated
}

fn target_for_kind(kind: AgentKind) -> super::agent_rules::AgentTarget {
    match kind {
        AgentKind::Claude => super::agent_rules::AgentTarget::ClaudeCode,
        AgentKind::Codex => super::agent_rules::AgentTarget::Codex,
        AgentKind::Cursor => super::agent_rules::AgentTarget::Cursor,
        AgentKind::Grok => super::agent_rules::AgentTarget::Grok,
        AgentKind::Pi => super::agent_rules::AgentTarget::Pi,
    }
}

pub(crate) fn hook_command(kind: AgentKind, track: bool, confirm: bool) -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    Ok(hook_command_for(
        &executable.to_string_lossy(),
        kind,
        track,
        confirm,
    ))
}

pub(crate) fn hook_command_for(exe: &str, kind: AgentKind, track: bool, confirm: bool) -> String {
    let mut command = format!("\"{exe}\" {MARKER} {}", kind.as_str());
    if track {
        command.push_str(" track");
    }
    if confirm {
        command.push_str(" confirm");
    }
    command
}

pub(crate) fn windows_hook_command_for(
    exe: &str,
    kind: AgentKind,
    track: bool,
    confirm: bool,
) -> String {
    let mut args = vec![MARKER, kind.as_str()];
    if track {
        args.push("track");
    }
    if confirm {
        args.push("confirm");
    }
    hook_edit::powershell_command(exe, &args)
}

fn install_handler(kind: AgentKind, track: bool, confirm: bool) -> Result<()> {
    let path = hook_path(kind);
    let original_bytes = std::fs::read(&path).ok();
    let original = original_bytes
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("{}");
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let updated = apply_handler_state(
        kind,
        original,
        &executable.to_string_lossy(),
        track,
        confirm,
    )?;
    hook_edit::atomic_write(&path, updated.as_bytes())?;
    if kind == AgentKind::Codex {
        if let Err(error) = super::agent_permission::reconcile_codex_trust(
            original,
            &updated,
            &[super::agent_lifecycle::MARKER, MARKER],
        ) {
            restore(&path, original_bytes.as_deref());
            return Err(error);
        }
    }
    Ok(())
}

fn remove_handler(kind: AgentKind) -> Result<()> {
    let path = hook_path(kind);
    let Ok(original) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let updated = apply_handler_state(kind, &original, "", false, false)?;
    hook_edit::atomic_write(&path, updated.as_bytes())?;
    if kind == AgentKind::Codex {
        if let Err(error) = super::agent_permission::reconcile_codex_trust(
            &original,
            &updated,
            &[super::agent_lifecycle::MARKER],
        ) {
            restore(&path, Some(original.as_bytes()));
            return Err(error);
        }
    }
    Ok(())
}

/// Pure JSONC reconciliation for the shared Stop handler. Lifecycle and confirmation are separate
/// product flags, but every supported agent must have at most one AskHuman-owned Stop command.
fn apply_handler_state(
    kind: AgentKind,
    original: &str,
    executable: &str,
    track: bool,
    confirm: bool,
) -> Result<String> {
    let without_lifecycle = match kind {
        AgentKind::Cursor => {
            hook_edit::remove_flat_marker(original, "stop", super::agent_lifecycle::MARKER)?
        }
        _ => hook_edit::remove_nested_marker(original, "Stop", super::agent_lifecycle::MARKER)?,
    };
    let without_stop = match kind {
        AgentKind::Cursor => hook_edit::remove_flat_marker(&without_lifecycle, "stop", MARKER)?,
        _ => hook_edit::remove_nested_marker(&without_lifecycle, "Stop", MARKER)?,
    };
    if !track && !confirm {
        return Ok(without_stop);
    }
    let command = hook_command_for(executable, kind, track, confirm);
    let command_windows = (kind == AgentKind::Codex)
        .then(|| windows_hook_command_for(executable, kind, track, confirm));
    match kind {
        AgentKind::Cursor => hook_edit::upsert_flat_handler(
            &without_stop,
            "stop",
            MARKER,
            &command,
            TIMEOUT_SECS,
            true,
        ),
        AgentKind::Codex => hook_edit::upsert_nested_group_with_windows(
            &without_stop,
            "Stop",
            MARKER,
            &command,
            command_windows.as_deref(),
            TIMEOUT_SECS,
            None,
        ),
        _ => hook_edit::upsert_nested_group(
            &without_stop,
            "Stop",
            MARKER,
            &command,
            TIMEOUT_SECS,
            None,
        ),
    }
}

fn hook_path(kind: AgentKind) -> std::path::PathBuf {
    match kind {
        AgentKind::Claude => crate::paths::claude_settings_json(),
        AgentKind::Codex => crate::paths::codex_hooks_json(),
        AgentKind::Cursor => crate::paths::cursor_hooks_json(),
        AgentKind::Grok => crate::paths::config_dir().join("unsupported-stop-hooks.json"),
        AgentKind::Pi => crate::paths::pi_extension_file(),
    }
}

fn stop_handlers(kind: AgentKind, text: &str) -> Vec<Value> {
    let value =
        jsonc_parser::parse_to_serde_value::<Value>(text, &jsonc_parser::ParseOptions::default())
            .ok();
    let event = if kind == AgentKind::Cursor {
        "stop"
    } else {
        "Stop"
    };
    let groups = value
        .as_ref()
        .and_then(|root| root.get("hooks"))
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if kind == AgentKind::Cursor {
        groups
    } else {
        groups
            .into_iter()
            .flat_map(|group| {
                group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    }
}

fn load_preferences() -> Preferences {
    std::fs::read(crate::paths::stop_preferences_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_preferences(preferences: &Preferences) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(preferences)?;
    hook_edit::atomic_write(&crate::paths::stop_preferences_file(), &bytes)
}

fn restore(path: &Path, bytes: Option<&[u8]>) {
    match bytes {
        Some(bytes) => {
            let _ = hook_edit::atomic_write(path, bytes);
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_matrix_and_default_off() {
        assert!(supported(AgentKind::Claude));
        assert!(supported(AgentKind::Codex));
        assert!(supported(AgentKind::Cursor));
        assert!(supported(AgentKind::Pi));
        assert!(!supported(AgentKind::Grok));
    }

    #[test]
    fn shared_handler_flags_can_be_reconciled_independently() {
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Cursor] {
            for mode in [
                super::super::agent_mode::Mode::None,
                super::super::agent_mode::Mode::Cli,
                super::super::agent_mode::Mode::Mcp,
            ] {
                for track in [false, true] {
                    let confirm = confirmation_active(true, mode);
                    let output =
                        apply_handler_state(kind, "{}", "/opt/AskHuman", track, confirm).unwrap();
                    let ours: Vec<Value> = stop_handlers(kind, &output)
                        .into_iter()
                        .filter(|handler| {
                            handler
                                .get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|command| command.contains(MARKER))
                        })
                        .collect();
                    assert_eq!(ours.len(), usize::from(track || confirm));
                    if let Some(handler) = ours.first() {
                        let command = handler.get("command").and_then(Value::as_str).unwrap();
                        assert_eq!(command.contains(" track"), track);
                        assert_eq!(command.contains(" confirm"), confirm);
                    }
                }
            }
        }
        assert!(!confirmation_active(
            false,
            super::super::agent_mode::Mode::Cli
        ));
    }

    #[test]
    fn disabled_confirmation_preserves_tracking_during_ordered_reconciliation() {
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Cursor] {
            for track in [false, true] {
                let legacy = apply_handler_state(kind, "{}", "/opt/AskHuman", track, true).unwrap();
                let expected = hook_command_for("/opt/AskHuman", kind, track, false);
                let expected_windows = (kind == AgentKind::Codex)
                    .then(|| windows_hook_command_for("/opt/AskHuman", kind, track, false));
                let stale = inspect_handler_state(
                    kind,
                    &legacy,
                    &expected,
                    expected_windows.as_deref(),
                    track,
                );
                assert!(stale.configured);
                assert!(stale.outdated);

                let cleaned =
                    apply_handler_state(kind, &legacy, "/opt/AskHuman", track, false).unwrap();
                let current = inspect_handler_state(
                    kind,
                    &cleaned,
                    &expected,
                    expected_windows.as_deref(),
                    track,
                );
                assert!(!current.configured);
                assert!(!current.outdated);
                assert_eq!(
                    stop_handlers(kind, &cleaned)
                        .iter()
                        .filter(|handler| {
                            handler
                                .get("command")
                                .and_then(Value::as_str)
                                .is_some_and(|command| command.contains(MARKER))
                        })
                        .count(),
                    usize::from(track)
                );
            }
        }
    }

    #[test]
    fn command_flags_cover_four_states() {
        let exe = "/opt/AskHuman";
        assert_eq!(
            hook_command_for(exe, AgentKind::Claude, false, false),
            "\"/opt/AskHuman\" __stop-hook claude"
        );
        assert!(hook_command_for(exe, AgentKind::Claude, true, false).ends_with("claude track"));
        assert!(hook_command_for(exe, AgentKind::Codex, false, true).ends_with("codex confirm"));
        assert!(
            hook_command_for(exe, AgentKind::Cursor, true, true).ends_with("cursor track confirm")
        );
    }

    #[test]
    fn parses_nested_and_flat_stop_handlers() {
        let nested = r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x __stop-hook claude confirm","timeout":86400}]}]}}"#;
        assert_eq!(stop_handlers(AgentKind::Claude, nested).len(), 1);
        let flat = r#"{"hooks":{"stop":[{"command":"x __stop-hook cursor confirm","timeout":86400,"loop_limit":null}]}}"#;
        assert_eq!(stop_handlers(AgentKind::Cursor, flat).len(), 1);
    }

    #[test]
    fn shared_handler_covers_full_flag_matrix_and_preserves_user_hooks() {
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Cursor] {
            let event = if kind == AgentKind::Cursor {
                "stop"
            } else {
                "Stop"
            };
            let input = if kind == AgentKind::Cursor {
                format!(
                    r#"{{ // keep
                      "version": 1,
                      "hooks": {{"{event}":[{{"command":"user-stop"}}]}}
                    }}"#
                )
            } else {
                format!(
                    r#"{{ // keep
                      "hooks": {{"{event}":[{{"hooks":[{{"type":"command","command":"user-stop"}}]}}]}}
                    }}"#
                )
            };
            for (track, confirm) in [(false, false), (true, false), (false, true), (true, true)] {
                let output =
                    apply_handler_state(kind, &input, "/opt/AskHuman", track, confirm).unwrap();
                assert!(output.contains("// keep"));
                let handlers = stop_handlers(kind, &output);
                assert!(handlers.iter().any(|handler| {
                    handler.get("command").and_then(Value::as_str) == Some("user-stop")
                }));
                let ours: Vec<&Value> = handlers
                    .iter()
                    .filter(|handler| {
                        handler
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| command.contains(MARKER))
                    })
                    .collect();
                if track || confirm {
                    assert_eq!(ours.len(), 1);
                    let handler = ours[0];
                    let expected = hook_command_for("/opt/AskHuman", kind, track, confirm);
                    assert_eq!(
                        handler.get("command").and_then(Value::as_str),
                        Some(expected.as_str())
                    );
                    if kind == AgentKind::Codex {
                        let expected_windows =
                            windows_hook_command_for("/opt/AskHuman", kind, track, confirm);
                        assert_eq!(
                            handler.get("commandWindows").and_then(Value::as_str),
                            Some(expected_windows.as_str())
                        );
                        assert_ne!(expected_windows, expected);
                    }
                    assert_eq!(
                        handler.get("timeout").and_then(Value::as_u64),
                        Some(TIMEOUT_SECS)
                    );
                    if kind == AgentKind::Cursor {
                        assert!(handler.get("loop_limit").is_some_and(Value::is_null));
                    }
                } else {
                    assert!(ours.is_empty());
                }
            }
        }
    }

    #[test]
    fn arbitrary_flag_switch_order_is_idempotent_and_keeps_one_owned_handler() {
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Cursor] {
            let mut text = "{}".to_string();
            for (track, confirm) in [
                (true, false),
                (true, true),
                (false, true),
                (true, true),
                (false, false),
                (false, true),
            ] {
                text = apply_handler_state(kind, &text, "/new/AskHuman", track, confirm).unwrap();
                let count = stop_handlers(kind, &text)
                    .iter()
                    .filter(|handler| {
                        handler
                            .get("command")
                            .and_then(Value::as_str)
                            .is_some_and(|command| command.contains(MARKER))
                    })
                    .count();
                assert_eq!(count, usize::from(track || confirm));
                let fixed =
                    apply_handler_state(kind, &text, "/new/AskHuman", track, confirm).unwrap();
                assert_eq!(fixed, text);
            }
        }
    }
}
