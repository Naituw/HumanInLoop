//! Managed `SubagentStart` context hook for Claude Code and Codex.

use super::agent_mode::Mode;
use super::agent_rules::AgentTarget;
use super::hook_edit;
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const MARKER: &str = "__subagent-hook";
pub const TIMEOUT_SECS: u64 = 10;

#[derive(Debug, Clone, Copy, Default)]
pub struct GuardStatus {
    pub installed: bool,
    pub outdated: bool,
}

pub fn supported(target: AgentTarget) -> bool {
    matches!(target, AgentTarget::ClaudeCode | AgentTarget::Codex)
}

pub fn status(target: AgentTarget) -> GuardStatus {
    if !supported(target) {
        return GuardStatus::default();
    }
    let path = hook_path(target);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".to_string());
    let groups = hook_edit::nested_groups(&text, "SubagentStart").unwrap_or_default();
    let expected = hook_command(target).unwrap_or_default();
    let expected_windows = (target == AgentTarget::Codex)
        .then(|| windows_hook_command(target))
        .transpose()
        .unwrap_or_default();
    let mut marker_count = 0usize;
    let mut exact_count = 0usize;
    for group in groups {
        let Some(handlers) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        for handler in handlers {
            let command = handler.get("command").and_then(Value::as_str).unwrap_or("");
            if !command.contains(MARKER) {
                continue;
            }
            marker_count += 1;
            if hook_edit::command_handler_matches(handler, &expected, expected_windows.as_deref())
                && handler.get("type").and_then(Value::as_str) == Some("command")
                && handler.get("timeout").and_then(Value::as_u64) == Some(TIMEOUT_SECS)
                && handler.get("statusMessage").is_none()
            {
                exact_count += 1;
            }
        }
    }
    let installed = marker_count > 0;
    let trust_ok = target != AgentTarget::Codex
        || (!installed
            || super::agent_permission::codex_marker_trusted(&path, MARKER).unwrap_or(false));
    GuardStatus {
        installed,
        outdated: installed && (marker_count != 1 || exact_count != 1 || !trust_ok),
    }
}

pub fn needs_update(target: AgentTarget, mode: Mode) -> bool {
    if !supported(target) {
        return false;
    }
    let current = status(target);
    match mode {
        Mode::None => current.installed,
        Mode::Cli | Mode::Mcp => !current.installed || current.outdated,
    }
}

pub(crate) fn reconcile_unlocked(target: AgentTarget, mode: Mode) -> Result<()> {
    if !supported(target) {
        return Ok(());
    }
    match mode {
        Mode::None => uninstall_unlocked(target),
        Mode::Cli | Mode::Mcp => install_unlocked(target),
    }
}

fn install_unlocked(target: AgentTarget) -> Result<()> {
    let path = hook_path(target);
    let original_hooks = std::fs::read(&path).ok();
    let original_config = (target == AgentTarget::Codex)
        .then(|| std::fs::read(crate::paths::codex_config_toml()).ok())
        .flatten();
    let existing = original_hooks
        .as_deref()
        .and_then(|bytes| std::str::from_utf8(bytes).ok())
        .unwrap_or("{}");
    let command = hook_command(target)?;
    let command_windows = (target == AgentTarget::Codex)
        .then(|| windows_hook_command(target))
        .transpose()?;
    let updated = hook_edit::upsert_nested_group_with_windows(
        existing,
        "SubagentStart",
        MARKER,
        &command,
        command_windows.as_deref(),
        TIMEOUT_SECS,
        None,
    )?;
    hook_edit::atomic_write(&path, updated.as_bytes())?;
    if target == AgentTarget::Codex {
        if let Err(error) =
            super::agent_permission::reconcile_codex_trust(existing, &updated, &[MARKER])
        {
            restore(&path, original_hooks.as_deref());
            restore(
                &crate::paths::codex_config_toml(),
                original_config.as_deref(),
            );
            return Err(error);
        }
    }
    Ok(())
}

fn uninstall_unlocked(target: AgentTarget) -> Result<()> {
    let path = hook_path(target);
    let Ok(existing) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let original_hooks = std::fs::read(&path).ok();
    let original_config = (target == AgentTarget::Codex)
        .then(|| std::fs::read(crate::paths::codex_config_toml()).ok())
        .flatten();
    let updated = hook_edit::remove_nested_marker(&existing, "SubagentStart", MARKER)?;
    hook_edit::atomic_write(&path, updated.as_bytes())?;
    if target == AgentTarget::Codex {
        if let Err(error) = super::agent_permission::reconcile_codex_trust(&existing, &updated, &[])
        {
            restore(&path, original_hooks.as_deref());
            restore(
                &crate::paths::codex_config_toml(),
                original_config.as_deref(),
            );
            return Err(error);
        }
    }
    Ok(())
}

fn hook_path(target: AgentTarget) -> PathBuf {
    match target {
        AgentTarget::ClaudeCode => crate::paths::claude_settings_json(),
        AgentTarget::Codex => crate::paths::codex_hooks_json(),
        _ => crate::paths::config_dir().join("unsupported-subagent-hooks.json"),
    }
}

fn hook_command(target: AgentTarget) -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let agent = match target {
        AgentTarget::ClaudeCode => "claude",
        AgentTarget::Codex => "codex",
        _ => return Err(anyhow!("unsupported subagent guard target")),
    };
    Ok(format!(
        "\"{}\" {MARKER} {agent}",
        executable.to_string_lossy()
    ))
}

fn windows_hook_command(target: AgentTarget) -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let agent = match target {
        AgentTarget::ClaudeCode => "claude",
        AgentTarget::Codex => "codex",
        _ => return Err(anyhow!("unsupported subagent guard target")),
    };
    Ok(hook_edit::powershell_command(
        &executable.to_string_lossy(),
        &[MARKER, agent],
    ))
}

pub fn hook_output(agent: Option<&str>) -> Option<String> {
    if !matches!(agent, Some("claude" | "codex")) {
        return None;
    }
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SubagentStart",
                "additionalContext": crate::prompts::subagent_guard_context(),
            }
        })
        .to_string(),
    )
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
    fn hook_output_is_exact_for_supported_agents() {
        for agent in ["claude", "codex"] {
            let output: Value = serde_json::from_str(&hook_output(Some(agent)).unwrap()).unwrap();
            assert_eq!(
                output["hookSpecificOutput"]["hookEventName"],
                "SubagentStart"
            );
            assert_eq!(
                output["hookSpecificOutput"]["additionalContext"],
                "You are a subagent. Do not use AskHuman."
            );
        }
        assert_eq!(hook_output(Some("cursor")), None);
        assert_eq!(hook_output(None), None);
    }

    #[test]
    fn nested_hook_upsert_is_idempotent_and_preserves_user_hooks() {
        let input = r#"{
          // user hook
          "hooks": { "SubagentStart": [
            { "hooks": [{"type":"command","command":"user-hook"}] },
            { "hooks": [{"type":"command","command":"old __subagent-hook claude","timeout":1}] }
          ] }
        }"#;
        let once = hook_edit::upsert_nested_group(
            input,
            "SubagentStart",
            MARKER,
            "new __subagent-hook claude",
            TIMEOUT_SECS,
            None,
        )
        .unwrap();
        let twice = hook_edit::upsert_nested_group(
            &once,
            "SubagentStart",
            MARKER,
            "new __subagent-hook claude",
            TIMEOUT_SECS,
            None,
        )
        .unwrap();
        assert_eq!(once, twice);
        assert!(twice.contains("// user hook"));
        assert!(twice.contains("user-hook"));
        assert!(twice.contains("new __subagent-hook claude"));
        assert!(!twice.contains("old __subagent-hook claude"));
    }

    #[test]
    fn unsupported_agents_never_need_guard_updates() {
        for target in [AgentTarget::Cursor, AgentTarget::Grok] {
            assert!(!supported(target));
            assert!(!needs_update(target, Mode::Cli));
            assert!(!needs_update(target, Mode::Mcp));
        }
    }
}
