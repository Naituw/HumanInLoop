//! Comment-preserving JSONC edits for nested command-hook groups.

use anyhow::{anyhow, Result};
use base64::Engine;
use jsonc_parser::cst::{CstNode, CstRootNode};
use jsonc_parser::json;
use jsonc_parser::ParseOptions;
use serde_json::Value;

pub fn command_handler_matches(
    handler: &Value,
    expected: &str,
    expected_windows: Option<&str>,
) -> bool {
    handler.get("command").and_then(Value::as_str) == Some(expected)
        && expected_windows.is_none_or(|expected_windows| {
            handler.get("commandWindows").and_then(Value::as_str) == Some(expected_windows)
        })
}

/// Build a Windows hook command that is independent of the outer session shell chosen by Codex.
/// `-EncodedCommand` avoids a second round of cmd.exe/PowerShell quoting and expansion for paths
/// containing spaces, apostrophes, dollar signs, or other shell metacharacters.
pub fn powershell_command(executable: &str, args: &[&str]) -> String {
    fn literal(value: &str) -> String {
        format!("'{}'", value.replace('\'', "''"))
    }

    let mut script = format!("& {}", literal(executable));
    for arg in args {
        script.push(' ');
        script.push_str(&literal(arg));
    }
    let utf16le: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let encoded = base64::engine::general_purpose::STANDARD.encode(utf16le);
    format!("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand {encoded}")
}

fn command_has_marker(value: &Value, marker: &str) -> bool {
    value
        .get("hooks")
        .and_then(Value::as_array)
        .map(|handlers| {
            handlers.iter().any(|handler| {
                ["command", "commandWindows"].iter().any(|field| {
                    handler
                        .get(*field)
                        .and_then(Value::as_str)
                        .is_some_and(|command| command.contains(marker))
                })
            })
        })
        .unwrap_or(false)
}

fn handler_node_has_marker(node: &CstNode, marker: &str) -> bool {
    node.to_serde_value()
        .map(|value: Value| {
            ["command", "commandWindows"].iter().any(|field| {
                value
                    .get(*field)
                    .and_then(Value::as_str)
                    .is_some_and(|command| command.contains(marker))
            })
        })
        .unwrap_or(false)
}

pub fn upsert_nested_group(
    text: &str,
    event: &str,
    marker: &str,
    command: &str,
    timeout: u64,
    status_message: Option<&str>,
) -> Result<String> {
    upsert_nested_group_with_windows(text, event, marker, command, None, timeout, status_message)
}

pub fn upsert_nested_group_with_windows(
    text: &str,
    event: &str,
    marker: &str,
    command: &str,
    command_windows: Option<&str>,
    timeout: u64,
    status_message: Option<&str>,
) -> Result<String> {
    let source = if text.trim().is_empty() { "{}" } else { text };
    let root = CstRootNode::parse(source, &ParseOptions::default())
        .map_err(|error| anyhow!("failed to parse hook config: {error}"))?;
    let root_object = root
        .object_value_or_create()
        .ok_or_else(|| anyhow!("hook config root is not an object"))?;
    let hooks = root_object
        .object_value_or_create("hooks")
        .ok_or_else(|| anyhow!("hook config 'hooks' is not an object"))?;
    let groups = hooks
        .array_value_or_create(event)
        .ok_or_else(|| anyhow!("hook event '{event}' is not an array"))?;
    let replacement_handler = match (command_windows, status_message) {
        (Some(command_windows), Some(message)) => json!({
            "type": "command",
            "command": command,
            "commandWindows": command_windows,
            "timeout": timeout,
            "statusMessage": message
        }),
        (Some(command_windows), None) => json!({
            "type": "command",
            "command": command,
            "commandWindows": command_windows,
            "timeout": timeout
        }),
        (None, Some(message)) => json!({
            "type": "command",
            "command": command,
            "timeout": timeout,
            "statusMessage": message
        }),
        (None, None) => json!({ "type": "command", "command": command, "timeout": timeout }),
    };
    let mut replaced = false;
    for group in groups.elements() {
        let Some(object) = group.as_object() else {
            continue;
        };
        let Some(handlers) = object.array_value("hooks") else {
            continue;
        };
        for handler in handlers.elements() {
            let has_marker = handler_node_has_marker(&handler, marker);
            if !has_marker {
                continue;
            }
            if !replaced {
                if let Some(handler_object) = handler.as_object() {
                    handler_object.replace_with(replacement_handler.clone());
                    replaced = true;
                    continue;
                }
            }
            handler.remove();
        }
    }
    if !replaced {
        groups.ensure_multiline();
        groups.append(json!({ "hooks": [replacement_handler] }));
    }
    Ok(root.to_string())
}

/// Replace or append one marker-owned nested group, including an optional group-level matcher.
/// Unlike [`upsert_nested_group`], this owns the whole group so stale matchers cannot survive.
pub fn upsert_nested_group_matched(
    text: &str,
    event: &str,
    marker: &str,
    matcher: Option<&str>,
    command: &str,
    timeout: u64,
) -> Result<String> {
    upsert_nested_group_matched_with_windows(text, event, marker, matcher, command, None, timeout)
}

pub fn upsert_nested_group_matched_with_windows(
    text: &str,
    event: &str,
    marker: &str,
    matcher: Option<&str>,
    command: &str,
    command_windows: Option<&str>,
    timeout: u64,
) -> Result<String> {
    let source = if text.trim().is_empty() { "{}" } else { text };
    let root = CstRootNode::parse(source, &ParseOptions::default())
        .map_err(|error| anyhow!("failed to parse hook config: {error}"))?;
    let root_object = root
        .object_value_or_create()
        .ok_or_else(|| anyhow!("hook config root is not an object"))?;
    let hooks = root_object
        .object_value_or_create("hooks")
        .ok_or_else(|| anyhow!("hook config 'hooks' is not an object"))?;
    let groups = hooks
        .array_value_or_create(event)
        .ok_or_else(|| anyhow!("hook event '{event}' is not an array"))?;
    let handler = match command_windows {
        Some(command_windows) => json!({
            "type": "command",
            "command": command,
            "commandWindows": command_windows,
            "timeout": timeout
        }),
        None => json!({ "type": "command", "command": command, "timeout": timeout }),
    };
    let replacement = match matcher {
        Some(matcher) => json!({ "matcher": matcher, "hooks": [handler] }),
        None => json!({ "hooks": [handler] }),
    };
    let mut replaced = false;
    for group in groups.elements() {
        let has_marker = group
            .to_serde_value()
            .map(|value| command_has_marker(&value, marker))
            .unwrap_or(false);
        if !has_marker {
            continue;
        }
        if !replaced {
            if let Some(object) = group.as_object() {
                object.replace_with(replacement.clone());
                replaced = true;
                continue;
            }
        }
        group.remove();
    }
    if !replaced {
        groups.ensure_multiline();
        groups.append(replacement);
    }
    Ok(root.to_string())
}

pub fn remove_nested_marker(text: &str, event: &str, marker: &str) -> Result<String> {
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|error| anyhow!("failed to parse hook config: {error}"))?;
    let Some(root_object) = root.object_value() else {
        return Ok(root.to_string());
    };
    let Some(hooks) = root_object.object_value("hooks") else {
        return Ok(root.to_string());
    };
    if let Some(groups) = hooks.array_value(event) {
        for group in groups.elements() {
            let Some(object) = group.as_object() else {
                continue;
            };
            let Some(handlers) = object.array_value("hooks") else {
                continue;
            };
            for handler in handlers.elements() {
                let has_marker = handler_node_has_marker(&handler, marker);
                if has_marker {
                    handler.remove();
                }
            }
            if handlers.elements().is_empty() {
                group.remove();
            }
        }
        if groups.elements().is_empty() {
            if let Some(property) = hooks.get(event) {
                property.remove();
            }
        }
    }
    Ok(root.to_string())
}

pub fn nested_groups(text: &str, event: &str) -> Result<Vec<Value>> {
    let value = jsonc_parser::parse_to_serde_value::<Value>(text, &ParseOptions::default())
        .map_err(|error| anyhow!("failed to parse hook config: {error}"))?;
    Ok(value
        .get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Idempotently replace or append a marker-owned Cursor flat hook in `hooks.<event>`.
pub fn upsert_flat_handler(
    text: &str,
    event: &str,
    marker: &str,
    command: &str,
    timeout: u64,
    unlimited_loop: bool,
) -> Result<String> {
    let source = if text.trim().is_empty() { "{}" } else { text };
    let root = CstRootNode::parse(source, &ParseOptions::default())
        .map_err(|error| anyhow!("failed to parse hook config: {error}"))?;
    let root_object = root
        .object_value_or_create()
        .ok_or_else(|| anyhow!("hook config root is not an object"))?;
    if root_object.get("version").is_none() {
        root_object.append("version", json!(1));
    }
    let hooks = root_object
        .object_value_or_create("hooks")
        .ok_or_else(|| anyhow!("hook config 'hooks' is not an object"))?;
    let handlers = hooks
        .array_value_or_create(event)
        .ok_or_else(|| anyhow!("hook event '{event}' is not an array"))?;
    let replacement = if unlimited_loop {
        json!({ "command": command, "timeout": timeout, "loop_limit": null })
    } else {
        json!({ "command": command, "timeout": timeout })
    };
    let mut replaced = false;
    for handler in handlers.elements() {
        if !handler_node_has_marker(&handler, marker) {
            continue;
        }
        if !replaced {
            if let Some(object) = handler.as_object() {
                object.replace_with(replacement.clone());
                replaced = true;
                continue;
            }
        }
        handler.remove();
    }
    if !replaced {
        handlers.ensure_multiline();
        handlers.append(replacement);
    }
    Ok(root.to_string())
}

pub fn remove_flat_marker(text: &str, event: &str, marker: &str) -> Result<String> {
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|error| anyhow!("failed to parse hook config: {error}"))?;
    let Some(root_object) = root.object_value() else {
        return Ok(root.to_string());
    };
    let Some(hooks) = root_object.object_value("hooks") else {
        return Ok(root.to_string());
    };
    if let Some(handlers) = hooks.array_value(event) {
        for handler in handlers.elements() {
            if handler_node_has_marker(&handler, marker) {
                handler.remove();
            }
        }
        if handlers.elements().is_empty() {
            if let Some(property) = hooks.get(event) {
                property.remove();
            }
        }
    }
    Ok(root.to_string())
}

pub fn group_has_marker(group: &Value, marker: &str) -> bool {
    command_has_marker(group, marker)
}

pub fn atomic_write(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Atomically write sensitive runtime state without ever exposing a world-readable temporary
/// file. The leaf directory and final file are restricted to the current user on Unix.
pub fn atomic_write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;

    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("private file path has no parent"))?;
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
    }

    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let write_result = (|| -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.flush()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result.map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_command_round_trips_shell_metacharacters_without_outer_quoting() {
        let command = powershell_command(
            r"C:\Users\O'Brien $dev\Ask Human.exe",
            &["__agent-hook", "codex", "activity"],
        );
        let encoded = command.split_whitespace().last().unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect();
        assert_eq!(
            String::from_utf16(&units).unwrap(),
            "& 'C:\\Users\\O''Brien $dev\\Ask Human.exe' '__agent-hook' 'codex' 'activity'"
        );
        assert!(!command.contains("Ask Human.exe"));
        assert!(command
            .starts_with("powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand "));
    }

    #[test]
    fn upsert_appends_and_preserves_other_groups_and_comments() {
        let input = r#"{
          // user hook
          "hooks": { "PermissionRequest": [
            { "matcher": "Bash", "hooks": [{"type":"command","command":"user"}] }
          ] }
        }"#;
        let output = upsert_nested_group(
            input,
            "PermissionRequest",
            "__permission-hook",
            "ask __permission-hook claude",
            90000,
            Some("Waiting for AskHuman permission approval"),
        )
        .unwrap();
        assert!(output.contains("// user hook"));
        let groups = nested_groups(&output, "PermissionRequest").unwrap();
        assert_eq!(groups.len(), 2);
        assert!(group_has_marker(&groups[1], "__permission-hook"));
    }

    #[test]
    fn flat_upsert_and_remove_preserve_user_handler() {
        let input = r#"{ // keep
          "version": 1,
          "hooks": { "stop": [
            {"command":"user-hook"},
            {"command":"old __stop-hook cursor"}
          ] }
        }"#;
        let output = upsert_flat_handler(
            input,
            "stop",
            "__stop-hook",
            "new __stop-hook cursor confirm",
            86400,
            true,
        )
        .unwrap();
        assert!(output.contains("// keep"));
        assert!(output.contains("user-hook"));
        assert!(output.contains("new __stop-hook cursor confirm"));
        assert!(!output.contains("old __stop-hook cursor"));
        let removed = remove_flat_marker(&output, "stop", "__stop-hook").unwrap();
        assert!(removed.contains("user-hook"));
        assert!(!removed.contains("__stop-hook"));
    }

    #[test]
    fn matched_group_upsert_owns_only_its_marker_group() {
        let input = r#"{
          // keep user and lifecycle groups
          "hooks": { "SessionStart": [
            {"matcher":"startup","hooks":[{"type":"command","command":"user-hook"}]},
            {"matcher":"old","hooks":[{"type":"command","command":"old __context-recovery-hook codex"}]},
            {"hooks":[{"type":"command","command":"AskHuman __agent-hook codex session-start"}]}
          ] }
        }"#;
        let output = upsert_nested_group_matched(
            input,
            "SessionStart",
            "__context-recovery-hook",
            Some("compact"),
            "AskHuman __context-recovery-hook codex session-start",
            30,
        )
        .unwrap();
        assert!(output.contains("// keep user and lifecycle groups"));
        assert!(output.contains("user-hook"));
        assert!(output.contains("__agent-hook codex session-start"));
        assert!(output.contains("\"matcher\": \"compact\""));
        assert!(!output.contains("\"matcher\":\"old\""));
        assert_eq!(output.matches("__context-recovery-hook").count(), 1);

        let removed =
            remove_nested_marker(&output, "SessionStart", "__context-recovery-hook").unwrap();
        assert!(removed.contains("user-hook"));
        assert!(removed.contains("__agent-hook codex session-start"));
        assert!(!removed.contains("__context-recovery-hook"));
    }

    #[test]
    fn private_atomic_write_overwrites_without_leaving_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let leaf = dir.path().join("private");
        let path = leaf.join("state.json");
        atomic_write_private(&path, b"first").unwrap();
        atomic_write_private(&path, b"second").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"second");
        assert_eq!(std::fs::read_dir(&leaf).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&leaf).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
