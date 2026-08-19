//! Claude Code question takeover (spec `docs/specs/claude-ask-user-question.md`).
//!
//! Claude's built-in `AskUserQuestion` asks the human a few multiple-choice questions. With this
//! capability on, a `PreToolUse` hook scoped to that tool hands the questions to AskHuman instead,
//! so they can be answered from the popup or any IM. The preference is stored independently, but
//! like the other capabilities the hook only lives on disk while the integration mode is CLI or MCP.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agents::AgentKind;

use super::hook_edit;

pub const MARKER: &str = "__ask-question-hook";
pub const TIMEOUT_SECS: u64 = 24 * 60 * 60;
/// Claude's hook event and the tool this capability is scoped to. The matcher keeps the entry from
/// touching any other tool, including the unscoped lifecycle `PreToolUse` entry.
const EVENT: &str = "PreToolUse";
const MATCHER: &str = "AskUserQuestion";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AskQuestionStatus {
    pub supported: bool,
    pub enabled: bool,
    pub installed: bool,
    pub outdated: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct Preferences {
    #[serde(default)]
    claude: Option<bool>,
}

/// Only Claude Code has a built-in question tool to take over.
pub fn supported(kind: AgentKind) -> bool {
    kind == AgentKind::Claude
}

/// Defaults to on: answering Claude's own questions remotely is the point of AskHuman.
pub fn enabled(kind: AgentKind) -> bool {
    supported(kind) && load_preferences().claude.unwrap_or(true)
}

pub fn set_enabled(kind: AgentKind, value: bool) -> Result<()> {
    if !supported(kind) {
        return Err(anyhow!("question takeover is unsupported for this agent"));
    }
    let _lock = super::mutation_lock::IntegrationMutationLock::acquire()?;
    let original = load_preferences();
    let mut preferences = original.clone();
    preferences.claude = Some(value);
    save_preferences(&preferences)?;
    if let Err(error) = reconcile_current_mode_unlocked(kind) {
        let _ = save_preferences(&original);
        return Err(error);
    }
    Ok(())
}

fn takeover_active(preference_enabled: bool, mode: super::agent_mode::Mode) -> bool {
    preference_enabled && mode != super::agent_mode::Mode::None
}

pub(crate) fn active_in_mode(kind: AgentKind, mode: super::agent_mode::Mode) -> bool {
    takeover_active(enabled(kind), mode)
}

pub(crate) fn active_in_current_mode(kind: AgentKind) -> bool {
    active_in_mode(kind, super::agent_mode::current(target_for_kind(kind)))
}

pub fn status(kind: AgentKind) -> AskQuestionStatus {
    if !supported(kind) {
        return AskQuestionStatus {
            supported: false,
            enabled: false,
            installed: false,
            outdated: false,
        };
    }
    let preference_enabled = enabled(kind);
    let desired = takeover_active(
        preference_enabled,
        super::agent_mode::current(target_for_kind(kind)),
    );
    let expected = hook_command().unwrap_or_default();
    let text = std::fs::read_to_string(hook_path(kind)).unwrap_or_else(|_| "{}".into());
    let (marker_count, exact_count) = inspect(&text, &expected);
    AskQuestionStatus {
        supported: true,
        enabled: preference_enabled,
        installed: marker_count > 0,
        outdated: if desired {
            marker_count != 1 || exact_count != 1
        } else {
            marker_count != 0
        },
    }
}

/// Counts AskHuman-owned entries for this capability and how many are already exactly as expected
/// (same command, matcher and timeout).
fn inspect(text: &str, expected: &str) -> (usize, usize) {
    let value =
        jsonc_parser::parse_to_serde_value::<Value>(text, &jsonc_parser::ParseOptions::default())
            .ok();
    let groups = value
        .as_ref()
        .and_then(|root| root.get("hooks"))
        .and_then(|hooks| hooks.get(EVENT))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut marker_count = 0usize;
    let mut exact_count = 0usize;
    for group in groups {
        let matcher_ok = group.get("matcher").and_then(Value::as_str) == Some(MATCHER);
        for handler in group
            .get("hooks")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            let command = handler.get("command").and_then(Value::as_str).unwrap_or("");
            if !command.contains(MARKER) {
                continue;
            }
            marker_count += 1;
            let timeout_ok = handler.get("timeout").and_then(Value::as_u64) == Some(TIMEOUT_SECS);
            if matcher_ok && command == expected && timeout_ok {
                exact_count += 1;
            }
        }
    }
    (marker_count, exact_count)
}

/// Reconcile after an integration mode change. Caller holds the integration mutation lock.
pub(crate) fn reconcile_unlocked(kind: AgentKind, mode: super::agent_mode::Mode) -> Result<()> {
    if !supported(kind) {
        return Ok(());
    }
    if active_in_mode(kind, mode) {
        install(kind)
    } else {
        remove(kind)
    }
}

pub(crate) fn reconcile_current_mode_unlocked(kind: AgentKind) -> Result<()> {
    reconcile_unlocked(kind, super::agent_mode::current(target_for_kind(kind)))
}

pub fn migrate_outdated() -> Vec<AgentKind> {
    let mut migrated = Vec::new();
    for kind in [AgentKind::Claude] {
        if status(kind).outdated {
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

pub(crate) fn hook_command() -> Result<String> {
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    Ok(hook_command_for(&executable.to_string_lossy()))
}

pub(crate) fn hook_command_for(exe: &str) -> String {
    format!("\"{exe}\" {MARKER} {}", AgentKind::Claude.as_str())
}

fn install(kind: AgentKind) -> Result<()> {
    let path = hook_path(kind);
    let original = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let updated = apply_state(&original, Some(&executable.to_string_lossy()))?;
    hook_edit::atomic_write(&path, updated.as_bytes())
}

fn remove(kind: AgentKind) -> Result<()> {
    let path = hook_path(kind);
    let Ok(original) = std::fs::read_to_string(&path) else {
        return Ok(());
    };
    let updated = apply_state(&original, None)?;
    hook_edit::atomic_write(&path, updated.as_bytes())
}

/// Pure JSONC reconciliation: drop our own entry, then re-add it when wanted. Other `PreToolUse`
/// entries (the user's own, and the unscoped lifecycle activity hook) are never touched.
fn apply_state(original: &str, executable: Option<&str>) -> Result<String> {
    let cleaned = hook_edit::remove_nested_marker(original, EVENT, MARKER)?;
    let Some(executable) = executable else {
        return Ok(cleaned);
    };
    hook_edit::upsert_nested_group_matched(
        &cleaned,
        EVENT,
        MARKER,
        Some(MATCHER),
        &hook_command_for(executable),
        TIMEOUT_SECS,
    )
}

fn hook_path(kind: AgentKind) -> std::path::PathBuf {
    match kind {
        AgentKind::Claude => crate::paths::claude_settings_json(),
        _ => crate::paths::config_dir().join("unsupported-ask-question-hooks.json"),
    }
}

fn load_preferences() -> Preferences {
    std::fs::read(crate::paths::ask_question_preferences_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn save_preferences(preferences: &Preferences) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(preferences)?;
    hook_edit::atomic_write(&crate::paths::ask_question_preferences_file(), &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = "/tmp/bin/AskHuman";

    #[test]
    fn install_scopes_the_entry_to_the_question_tool() {
        let updated = apply_state("{}", Some(EXE)).unwrap();
        let value: Value = serde_json::from_str(&updated).unwrap();
        let group = &value["hooks"][EVENT][0];
        assert_eq!(group["matcher"], MATCHER);
        assert_eq!(
            group["hooks"][0]["command"],
            Value::from(hook_command_for(EXE))
        );
        assert_eq!(group["hooks"][0]["timeout"], Value::from(TIMEOUT_SECS));

        let (marker_count, exact_count) = inspect(&updated, &hook_command_for(EXE));
        assert_eq!((marker_count, exact_count), (1, 1));
    }

    #[test]
    fn install_is_idempotent_and_leaves_other_entries_alone() {
        let original = r#"{
  "hooks": {
    "PreToolUse": [
      { "hooks": [ { "type": "command", "command": "\"/tmp/bin/AskHuman\" __agent-hook claude activity" } ] },
      { "matcher": "Bash", "hooks": [ { "type": "command", "command": "user-own-hook.sh" } ] }
    ]
  }
}"#;
        let once = apply_state(original, Some(EXE)).unwrap();
        let twice = apply_state(&once, Some(EXE)).unwrap();
        assert_eq!(once, twice);

        let value: Value = serde_json::from_str(&twice).unwrap();
        let groups = value["hooks"][EVENT].as_array().unwrap();
        assert_eq!(groups.len(), 3);
        assert!(groups.iter().any(|group| group["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default()
            .contains("__agent-hook claude activity")));
        assert!(groups
            .iter()
            .any(|group| group["hooks"][0]["command"] == "user-own-hook.sh"));
        assert_eq!(inspect(&twice, &hook_command_for(EXE)), (1, 1));
    }

    #[test]
    fn removal_drops_only_our_entry() {
        let installed = apply_state(
            r#"{ "hooks": { "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "user-own-hook.sh" } ] } ] } }"#,
            Some(EXE),
        )
        .unwrap();
        let removed = apply_state(&installed, None).unwrap();
        let value: Value = serde_json::from_str(&removed).unwrap();
        let groups = value["hooks"][EVENT].as_array().unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0]["hooks"][0]["command"], "user-own-hook.sh");
        assert_eq!(inspect(&removed, &hook_command_for(EXE)), (0, 0));
    }

    #[test]
    fn a_stale_command_path_counts_as_outdated() {
        let installed = apply_state("{}", Some("/old/path/AskHuman")).unwrap();
        let (marker_count, exact_count) = inspect(&installed, &hook_command_for(EXE));
        assert_eq!(marker_count, 1);
        assert_eq!(exact_count, 0);
    }

    #[test]
    fn takeover_needs_an_integration_mode() {
        assert!(takeover_active(true, super::super::agent_mode::Mode::Cli));
        assert!(!takeover_active(true, super::super::agent_mode::Mode::None));
        assert!(!takeover_active(false, super::super::agent_mode::Mode::Cli));
    }
}
