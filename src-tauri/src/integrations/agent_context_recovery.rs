//! Integration-mode-owned hooks for post-compaction prompting and MCP session binding.

use super::agent_mode::Mode;
use super::agent_rules::AgentTarget;
use super::hook_edit;
use anyhow::{Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MARKER: &str = "__context-recovery-hook";
pub const TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryStatus {
    pub installed: bool,
    pub outdated: bool,
}

#[derive(Clone, Copy)]
struct EventSpec {
    event: &'static str,
    runtime_event: &'static str,
    matcher: Option<&'static str>,
    flat: bool,
}

const SESSION_START: EventSpec = EventSpec {
    event: "SessionStart",
    runtime_event: "session-start",
    matcher: Some("compact"),
    flat: false,
};
const PRE_TOOL_NESTED: EventSpec = EventSpec {
    event: "PreToolUse",
    runtime_event: "pre-tool-use",
    matcher: None,
    flat: false,
};
const PRE_TOOL_CURSOR: EventSpec = EventSpec {
    event: "preToolUse",
    runtime_event: "pre-tool-use",
    matcher: None,
    flat: true,
};

fn all_specs(target: AgentTarget) -> &'static [EventSpec] {
    match target {
        AgentTarget::ClaudeCode => &[SESSION_START, PRE_TOOL_NESTED],
        AgentTarget::Codex => &[SESSION_START],
        AgentTarget::Cursor => &[PRE_TOOL_CURSOR],
        AgentTarget::Grok => &[PRE_TOOL_NESTED],
        AgentTarget::Pi => &[],
    }
}

fn desired(target: AgentTarget, mode: Mode, spec: EventSpec) -> bool {
    match mode {
        Mode::None => false,
        Mode::Cli => {
            matches!(target, AgentTarget::ClaudeCode | AgentTarget::Codex)
                && spec.runtime_event == "session-start"
        }
        Mode::Mcp => match target {
            AgentTarget::ClaudeCode => true,
            AgentTarget::Codex => spec.runtime_event == "session-start",
            AgentTarget::Cursor | AgentTarget::Grok => spec.runtime_event == "pre-tool-use",
            AgentTarget::Pi => false,
        },
    }
}

pub fn supported() -> bool {
    true
}

pub fn status(target: AgentTarget, mode: Mode) -> RecoveryStatus {
    if !supported() || target == AgentTarget::Pi {
        return RecoveryStatus::default();
    }
    let text = std::fs::read_to_string(hook_path(target)).unwrap_or_else(|_| "{}".into());
    let mut result = status_from_text(target, mode, &text, true);
    if target == AgentTarget::Codex
        && result.installed
        && !super::agent_permission::codex_marker_trusted(&hook_path(target), MARKER)
            .unwrap_or(false)
    {
        result.outdated = true;
    }
    result
}

fn status_from_text(target: AgentTarget, mode: Mode, text: &str, trust_ok: bool) -> RecoveryStatus {
    let Ok(value) =
        jsonc_parser::parse_to_serde_value::<Value>(text, &jsonc_parser::ParseOptions::default())
    else {
        return RecoveryStatus {
            installed: false,
            outdated: true,
        };
    };
    if !value.is_object() {
        return RecoveryStatus {
            installed: false,
            outdated: true,
        };
    }
    let mut marker_count = 0usize;
    let mut exact_count = 0usize;
    let mut desired_count = 0usize;
    for spec in all_specs(target) {
        let entries = value
            .get("hooks")
            .and_then(|hooks| hooks.get(spec.event))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for entry in entries {
            let (command, command_windows, timeout, matcher) = if spec.flat {
                (
                    entry.get("command").and_then(Value::as_str),
                    None,
                    entry.get("timeout").and_then(Value::as_u64),
                    None,
                )
            } else {
                let handler = entry
                    .get("hooks")
                    .and_then(Value::as_array)
                    .and_then(|handlers| handlers.first());
                (
                    handler
                        .and_then(|handler| handler.get("command"))
                        .and_then(Value::as_str),
                    handler
                        .and_then(|handler| handler.get("commandWindows"))
                        .and_then(Value::as_str),
                    handler
                        .and_then(|handler| handler.get("timeout"))
                        .and_then(Value::as_u64),
                    entry.get("matcher").and_then(Value::as_str),
                )
            };
            if !command.is_some_and(|command| command.contains(MARKER)) {
                continue;
            }
            marker_count += 1;
            if desired(target, mode, *spec) {
                let expected = hook_command(target, spec.runtime_event).unwrap_or_default();
                let expected_windows = (target == AgentTarget::Codex)
                    .then(|| windows_hook_command(target, spec.runtime_event))
                    .transpose()
                    .unwrap_or_default();
                let windows_override_ok = expected_windows
                    .as_deref()
                    .is_none_or(|expected_windows| command_windows == Some(expected_windows));
                if command == Some(expected.as_str())
                    && windows_override_ok
                    && timeout == Some(TIMEOUT_SECS)
                    && matcher == spec.matcher
                {
                    exact_count += 1;
                }
            }
        }
        if desired(target, mode, *spec) {
            desired_count += 1;
        }
    }
    let installed = marker_count > 0;
    RecoveryStatus {
        installed,
        outdated: marker_count != desired_count || exact_count != desired_count || !trust_ok,
    }
}

pub fn needs_update(target: AgentTarget, mode: Mode) -> bool {
    if !supported() || target == AgentTarget::Pi {
        return false;
    }
    status(target, mode).outdated
}

pub(crate) fn reconcile_unlocked(target: AgentTarget, mode: Mode) -> Result<()> {
    if !supported() || target == AgentTarget::Pi {
        return Ok(());
    }
    let path = hook_path(target);
    let original_hooks = std::fs::read(&path).ok();
    let original_config = (target == AgentTarget::Codex)
        .then(|| std::fs::read(crate::paths::codex_config_toml()).ok())
        .flatten();
    let original_text = existing_hook_text(original_hooks.as_deref())?;
    let updated = reconcile_text(target, mode, original_text)?;
    if updated == "{}" && original_hooks.is_none() {
        return Ok(());
    }
    let hooks_changed = updated != original_text;
    if hooks_changed {
        hook_edit::atomic_write(&path, updated.as_bytes())?;
    }
    if target == AgentTarget::Codex {
        let trust_markers: &[&str] = if mode == Mode::None { &[] } else { &[MARKER] };
        if let Err(error) =
            super::agent_permission::reconcile_codex_trust(original_text, &updated, trust_markers)
        {
            if hooks_changed {
                restore(&path, original_hooks.as_deref());
            }
            restore(
                &crate::paths::codex_config_toml(),
                original_config.as_deref(),
            );
            return Err(error);
        }
    }
    Ok(())
}

fn existing_hook_text(bytes: Option<&[u8]>) -> Result<&str> {
    match bytes {
        Some(bytes) => std::str::from_utf8(bytes).context("hook config is not valid UTF-8"),
        None => Ok("{}"),
    }
}

fn reconcile_text(target: AgentTarget, mode: Mode, text: &str) -> Result<String> {
    if !status_from_text(target, mode, text, true).outdated {
        return Ok(text.to_string());
    }
    let mut updated = text.to_string();
    for spec in all_specs(target) {
        if desired(target, mode, *spec) {
            let command = hook_command(target, spec.runtime_event)?;
            let command_windows = (target == AgentTarget::Codex)
                .then(|| windows_hook_command(target, spec.runtime_event))
                .transpose()?;
            updated = if spec.flat {
                hook_edit::upsert_flat_handler(
                    &updated,
                    spec.event,
                    MARKER,
                    &command,
                    TIMEOUT_SECS,
                    false,
                )?
            } else {
                hook_edit::upsert_nested_group_matched_with_windows(
                    &updated,
                    spec.event,
                    MARKER,
                    spec.matcher,
                    &command,
                    command_windows.as_deref(),
                    TIMEOUT_SECS,
                )?
            };
        } else {
            updated = if spec.flat {
                hook_edit::remove_flat_marker(&updated, spec.event, MARKER)?
            } else {
                hook_edit::remove_nested_marker(&updated, spec.event, MARKER)?
            };
        }
    }
    Ok(updated)
}

fn hook_path(target: AgentTarget) -> PathBuf {
    match target {
        AgentTarget::ClaudeCode => crate::paths::claude_settings_json(),
        AgentTarget::Codex => crate::paths::codex_hooks_json(),
        AgentTarget::Cursor => crate::paths::cursor_hooks_json(),
        AgentTarget::Grok => crate::paths::grok_hooks_json(),
        AgentTarget::Pi => crate::paths::pi_extension_file(),
    }
}

fn hook_command(target: AgentTarget, event: &str) -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let agent = match target {
        AgentTarget::ClaudeCode => "claude",
        AgentTarget::Codex => "codex",
        AgentTarget::Cursor => "cursor",
        AgentTarget::Grok => "grok",
        AgentTarget::Pi => "pi",
    };
    Ok(format!(
        "\"{}\" {MARKER} {agent} {event}",
        executable.to_string_lossy()
    ))
}

fn windows_hook_command(target: AgentTarget, event: &str) -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let agent = match target {
        AgentTarget::ClaudeCode => "claude",
        AgentTarget::Codex => "codex",
        AgentTarget::Cursor => "cursor",
        AgentTarget::Grok => "grok",
        AgentTarget::Pi => "pi",
    };
    Ok(hook_edit::powershell_command(
        &executable.to_string_lossy(),
        &[MARKER, agent, event],
    ))
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

    fn desired_events(target: AgentTarget, mode: Mode) -> Vec<&'static str> {
        all_specs(target)
            .iter()
            .filter(|spec| desired(target, mode, **spec))
            .map(|spec| spec.runtime_event)
            .collect()
    }

    #[test]
    fn desired_matrix_is_mode_specific() {
        let cases = [
            (AgentTarget::ClaudeCode, Mode::None, vec![]),
            (AgentTarget::ClaudeCode, Mode::Cli, vec!["session-start"]),
            (
                AgentTarget::ClaudeCode,
                Mode::Mcp,
                vec!["session-start", "pre-tool-use"],
            ),
            (AgentTarget::Codex, Mode::None, vec![]),
            (AgentTarget::Codex, Mode::Cli, vec!["session-start"]),
            (AgentTarget::Codex, Mode::Mcp, vec!["session-start"]),
            (AgentTarget::Cursor, Mode::None, vec![]),
            (AgentTarget::Cursor, Mode::Cli, vec![]),
            (AgentTarget::Cursor, Mode::Mcp, vec!["pre-tool-use"]),
            (AgentTarget::Grok, Mode::None, vec![]),
            (AgentTarget::Grok, Mode::Cli, vec![]),
            (AgentTarget::Grok, Mode::Mcp, vec!["pre-tool-use"]),
        ];
        for (target, mode, expected) in cases {
            assert_eq!(
                desired_events(target, mode),
                expected,
                "{target:?} {mode:?}"
            );
        }
    }

    #[test]
    fn reconcile_and_status_cover_every_supported_mode_shape() {
        for (target, mode) in [
            (AgentTarget::ClaudeCode, Mode::Cli),
            (AgentTarget::ClaudeCode, Mode::Mcp),
            (AgentTarget::Codex, Mode::Cli),
            (AgentTarget::Codex, Mode::Mcp),
            (AgentTarget::Cursor, Mode::Mcp),
            (AgentTarget::Grok, Mode::Mcp),
        ] {
            let text = reconcile_text(target, mode, "{}").unwrap();
            assert_eq!(
                status_from_text(target, mode, &text, true),
                RecoveryStatus {
                    installed: true,
                    outdated: false,
                },
                "{target:?} {mode:?}: {text}"
            );
            let removed = reconcile_text(target, Mode::None, &text).unwrap();
            assert_eq!(
                status_from_text(target, Mode::None, &removed, true),
                RecoveryStatus {
                    installed: false,
                    outdated: false,
                },
                "{target:?}: {removed}"
            );
        }
        for target in [AgentTarget::Cursor, AgentTarget::Grok] {
            assert_eq!(
                status_from_text(target, Mode::Cli, "{}", true),
                RecoveryStatus {
                    installed: false,
                    outdated: false,
                }
            );
        }
    }

    #[test]
    fn status_detects_missing_stale_duplicate_and_untrusted_hooks() {
        assert_eq!(
            status_from_text(AgentTarget::ClaudeCode, Mode::Mcp, "{}", true),
            RecoveryStatus {
                installed: false,
                outdated: true,
            }
        );
        let exact = reconcile_text(AgentTarget::Codex, Mode::Mcp, "{}").unwrap();
        assert_eq!(
            status_from_text(AgentTarget::Codex, Mode::Mcp, &exact, false),
            RecoveryStatus {
                installed: true,
                outdated: true,
            }
        );
        let stale = exact.replace(&format!("\"timeout\": {TIMEOUT_SECS}"), "\"timeout\": 1");
        assert!(status_from_text(AgentTarget::Codex, Mode::Mcp, &stale, true).outdated);
        let command = hook_command(AgentTarget::Codex, "session-start").unwrap();
        let duplicate = serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {
                        "matcher": "compact",
                        "hooks": [{"type":"command", "command": command, "timeout": TIMEOUT_SECS}]
                    },
                    {
                        "matcher": "compact",
                        "hooks": [{
                            "type":"command",
                            "command": format!("duplicate {MARKER}"),
                            "timeout": TIMEOUT_SECS
                        }]
                    }
                ]
            }
        })
        .to_string();
        assert!(status_from_text(AgentTarget::Codex, Mode::Mcp, &duplicate, true).outdated);
        assert!(status_from_text(AgentTarget::Cursor, Mode::Mcp, "not json", true).outdated);
        assert!(status_from_text(AgentTarget::Cursor, Mode::None, "[]", true).outdated);
    }

    #[test]
    fn reconcile_preserves_user_and_lifecycle_hooks_and_is_idempotent() {
        let original = r#"{
          // keep this comment
          "hooks": {
            "SessionStart": [{"matcher":"startup","hooks":[{"type":"command","command":"user-start"}]}],
            "PreToolUse": [{"hooks":[{"type":"command","command":"AskHuman __agent-hook claude activity"}]}]
          }
        }"#;
        let installed = reconcile_text(AgentTarget::ClaudeCode, Mode::Mcp, original).unwrap();
        assert!(installed.contains("// keep this comment"));
        assert!(installed.contains("user-start"));
        assert!(installed.contains("__agent-hook claude activity"));
        assert_eq!(installed.matches(MARKER).count(), 2);
        assert_eq!(
            reconcile_text(AgentTarget::ClaudeCode, Mode::Mcp, &installed).unwrap(),
            installed
        );

        let cli = reconcile_text(AgentTarget::ClaudeCode, Mode::Cli, &installed).unwrap();
        assert_eq!(cli.matches(MARKER).count(), 1);
        assert!(cli.contains("user-start"));
        assert!(cli.contains("__agent-hook claude activity"));

        let removed = reconcile_text(AgentTarget::ClaudeCode, Mode::None, &cli).unwrap();
        assert!(!removed.contains(MARKER));
        assert!(removed.contains("user-start"));
        assert!(removed.contains("__agent-hook claude activity"));
        assert!(reconcile_text(AgentTarget::ClaudeCode, Mode::Mcp, "[]").is_err());
        assert!(reconcile_text(AgentTarget::Cursor, Mode::None, "not json").is_err());
    }

    #[test]
    fn existing_hook_text_rejects_invalid_utf8_instead_of_overwriting_it() {
        assert_eq!(existing_hook_text(None).unwrap(), "{}");
        assert_eq!(
            existing_hook_text(Some(b"{\"hooks\":{}}")).unwrap(),
            r#"{"hooks":{}}"#
        );
        assert!(existing_hook_text(Some(&[0xff, 0xfe])).is_err());
    }
}
