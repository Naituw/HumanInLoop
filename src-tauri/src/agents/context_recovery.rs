//! Runtime for the integration-mode-owned context recovery hook.

use super::AgentKind;
use crate::integrations::agent_mode::Mode;
use serde_json::Value;
use std::collections::HashMap;

pub fn run(args: &[String]) {
    let Some(kind) = args.first().and_then(|value| AgentKind::parse(value)) else {
        return;
    };
    let Some(event) = args.get(1).map(String::as_str) else {
        return;
    };
    let env: HashMap<String, String> = std::env::vars().collect();
    if super::report::should_skip(kind, &env) {
        return;
    }
    let input = super::report::read_stdin_json();
    let output = match event {
        "session-start" => session_start_output(kind, mode(kind), input.as_ref()),
        "pre-tool-use" => pre_tool_output(kind, input.as_ref(), &env),
        _ => None,
    };
    if let Some(output) = output {
        println!("{output}");
    }
}

fn mode(kind: AgentKind) -> Mode {
    crate::integrations::agent_mode::current(match kind {
        AgentKind::Claude => crate::integrations::agent_rules::AgentTarget::ClaudeCode,
        AgentKind::Codex => crate::integrations::agent_rules::AgentTarget::Codex,
        AgentKind::Cursor => crate::integrations::agent_rules::AgentTarget::Cursor,
        AgentKind::Grok => crate::integrations::agent_rules::AgentTarget::Grok,
        AgentKind::Pi => crate::integrations::agent_rules::AgentTarget::Pi,
    })
}

fn session_start_output(kind: AgentKind, mode: Mode, input: Option<&Value>) -> Option<String> {
    if !matches!(kind, AgentKind::Claude | AgentKind::Codex) || !is_compact_session_start(input?) {
        return None;
    }
    let prompt = match mode {
        Mode::Cli => crate::prompts::compact_recovery_cli_prompt(),
        Mode::Mcp => crate::prompts::compact_recovery_mcp_prompt(),
        Mode::None => return None,
    };
    Some(
        serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": prompt,
            }
        })
        .to_string(),
    )
}

fn is_compact_session_start(input: &Value) -> bool {
    ["source", "session_start_source", "sessionStartSource"]
        .into_iter()
        .filter_map(|key| input.get(key).and_then(Value::as_str))
        .any(|source| source.eq_ignore_ascii_case("compact"))
}

fn pre_tool_output(
    kind: AgentKind,
    input: Option<&Value>,
    env: &HashMap<String, String>,
) -> Option<String> {
    pre_tool_output_for_mode(kind, mode(kind), input, env)
}

fn pre_tool_output_for_mode(
    kind: AgentKind,
    current_mode: Mode,
    input: Option<&Value>,
    env: &HashMap<String, String>,
) -> Option<String> {
    if current_mode != Mode::Mcp {
        return None;
    }
    let input = input?;
    if input
        .get("toolInputTruncated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let raw_name = super::report::tool_name(input)?;
    let tool_name = askhuman_tool_name(kind, &raw_name)?;
    let tool_input = super::report::tool_input(input)?;
    let arguments = arguments_for(kind, &raw_name, &tool_input)?;
    if !input_shape_matches(tool_name, arguments) {
        return None;
    }
    let session_id = session_id_from_hook(kind, input, env)?;

    match kind {
        AgentKind::Claude | AgentKind::Cursor => {
            let token =
                crate::context_binding::create_token(kind.as_str(), &session_id, tool_name)?;
            binding_output(kind, arguments, token)
        }
        AgentKind::Grok => {
            crate::context_binding::record_grok_pending(
                &session_id,
                &raw_name,
                tool_name,
                arguments,
                input,
                env,
            );
            None
        }
        // Pi recovery is injected by the managed extension's `before_agent_start` handler.
        AgentKind::Pi => None,
        AgentKind::Codex => None,
    }
}

fn binding_output(kind: AgentKind, arguments: &Value, token: String) -> Option<String> {
    let mut updated = arguments.as_object()?.clone();
    updated.insert(
        crate::context_binding::HIDDEN_TOKEN_FIELD.to_string(),
        Value::String(token),
    );
    Some(match kind {
        AgentKind::Claude => serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "updatedInput": Value::Object(updated),
            }
        })
        .to_string(),
        AgentKind::Cursor => serde_json::json!({
            "updated_input": {
                (crate::context_binding::HIDDEN_TOKEN_FIELD): updated
                    .remove(crate::context_binding::HIDDEN_TOKEN_FIELD)
                    .expect("inserted token"),
            }
        })
        .to_string(),
        _ => return None,
    })
}

fn askhuman_tool_name(kind: AgentKind, raw: &str) -> Option<&'static str> {
    let prefix = match kind {
        AgentKind::Claude => "mcp__askhuman__",
        AgentKind::Cursor => "MCP:",
        AgentKind::Grok => "askhuman__",
        AgentKind::Codex | AgentKind::Pi => return None,
    };
    let name = raw.strip_prefix(prefix)?;
    match name {
        "ask" => Some("ask"),
        "whats_next" => Some("whats_next"),
        "show_last" => Some("show_last"),
        _ => None,
    }
}

fn arguments_for<'a>(kind: AgentKind, raw_name: &str, tool_input: &'a Value) -> Option<&'a Value> {
    if kind != AgentKind::Grok {
        return tool_input.is_object().then_some(tool_input);
    }
    // Grok Build wraps MCP input. Validate both copies of the qualified name before hashing.
    if let Some(wrapper_name) = tool_input.get("tool_name").and_then(Value::as_str) {
        if wrapper_name != raw_name {
            return None;
        }
        return tool_input
            .get("tool_input")
            .filter(|value| value.is_object());
    }
    // Composer/future builds may deliver direct arguments. Accept only an object shape.
    tool_input.is_object().then_some(tool_input)
}

fn input_shape_matches(tool_name: &str, input: &Value) -> bool {
    let Some(object) = input.as_object() else {
        return false;
    };
    let allowed: &[&str] = match tool_name {
        "ask" => &["message", "questions", "files"],
        "whats_next" => &["message", "options", "files"],
        "show_last" => &["count"],
        _ => return false,
    };
    if object.keys().any(|key| !allowed.contains(&key.as_str())) {
        return false;
    }
    match tool_name {
        "ask" => {
            object
                .get("message")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
                || object
                    .get("questions")
                    .and_then(Value::as_array)
                    .is_some_and(|questions| !questions.is_empty())
        }
        "show_last" => match object.get("count") {
            None => true,
            Some(Value::Number(n)) => n.as_u64().is_some_and(|v| (1..=10).contains(&v)),
            _ => false,
        },
        "whats_next" => true,
        _ => false,
    }
}

fn session_id_from_hook(
    kind: AgentKind,
    input: &Value,
    env: &HashMap<String, String>,
) -> Option<String> {
    for key in [
        "session_id",
        "sessionId",
        "conversation_id",
        "conversationId",
        "thread_id",
        "threadId",
    ] {
        if let Some(value) = input
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            return Some(value.to_string());
        }
    }
    super::detect::session_id_from_env_map(kind, env)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn compact_prompt_strictly_follows_selected_mode() {
        let input = json!({"source": "compact"});
        let cli = session_start_output(AgentKind::Codex, Mode::Cli, Some(&input)).unwrap();
        assert!(cli.contains("AskHuman --show-last"));
        assert!(!cli.contains("MCP `show_last`"));
        let mcp = session_start_output(AgentKind::Claude, Mode::Mcp, Some(&input)).unwrap();
        assert!(mcp.contains("MCP `show_last`"));
        assert!(!mcp.contains("AskHuman --show-last"));
        assert!(session_start_output(AgentKind::Codex, Mode::None, Some(&input)).is_none());
    }

    #[test]
    fn compact_prompt_requires_supported_agent_and_compact_source() {
        for input in [
            json!({"source": "COMPACT"}),
            json!({"session_start_source": "compact"}),
            json!({"sessionStartSource": "Compact"}),
        ] {
            assert!(session_start_output(AgentKind::Claude, Mode::Cli, Some(&input)).is_some());
        }
        for input in [
            json!({}),
            json!({"source": "startup"}),
            json!({"source": 1}),
        ] {
            assert!(session_start_output(AgentKind::Codex, Mode::Cli, Some(&input)).is_none());
        }
        assert!(session_start_output(
            AgentKind::Cursor,
            Mode::Mcp,
            Some(&json!({"source":"compact"}))
        )
        .is_none());
        assert!(session_start_output(
            AgentKind::Grok,
            Mode::Mcp,
            Some(&json!({"source":"compact"}))
        )
        .is_none());
        assert!(session_start_output(AgentKind::Claude, Mode::Cli, None).is_none());
    }

    #[test]
    fn exact_tool_names_and_input_guards() {
        assert_eq!(
            askhuman_tool_name(AgentKind::Claude, "mcp__askhuman__show_last"),
            Some("show_last")
        );
        assert_eq!(
            askhuman_tool_name(AgentKind::Cursor, "MCP:ask"),
            Some("ask")
        );
        assert!(askhuman_tool_name(AgentKind::Cursor, "MCP:other").is_none());
        assert!(input_shape_matches("show_last", &json!({})));
        assert!(input_shape_matches("show_last", &json!({"count": 3})));
        assert!(!input_shape_matches("show_last", &json!({"count": 0})));
        assert!(!input_shape_matches("show_last", &json!({"x": 1})));
        assert!(input_shape_matches("ask", &json!({"message": "hello"})));
        assert!(!input_shape_matches("ask", &json!({"message": ""})));
        assert!(input_shape_matches(
            "ask",
            &json!({"questions": [{"question": "Continue?"}]})
        ));
        assert!(input_shape_matches("whats_next", &json!({})));
        assert!(input_shape_matches(
            "whats_next",
            &json!({"message": "done", "options": [], "files": []})
        ));
        assert!(!input_shape_matches(
            "whats_next",
            &json!({"unexpected": true})
        ));
        assert!(!input_shape_matches("ask", &json!([])));

        for name in ["ask", "whats_next", "show_last"] {
            assert_eq!(
                askhuman_tool_name(AgentKind::Claude, &format!("mcp__askhuman__{name}")),
                Some(name)
            );
            assert_eq!(
                askhuman_tool_name(AgentKind::Cursor, &format!("MCP:{name}")),
                Some(name)
            );
            assert_eq!(
                askhuman_tool_name(AgentKind::Grok, &format!("askhuman__{name}")),
                Some(name)
            );
        }
        assert!(askhuman_tool_name(AgentKind::Codex, "mcp__askhuman__ask").is_none());
        assert!(askhuman_tool_name(AgentKind::Claude, "MCP:ask").is_none());
        assert!(askhuman_tool_name(AgentKind::Cursor, "mcp:ask").is_none());
    }

    #[test]
    fn grok_wrapper_requires_matching_qualified_name() {
        let good = json!({
            "tool_name": "askhuman__show_last",
            "tool_input": {}
        });
        assert_eq!(
            arguments_for(AgentKind::Grok, "askhuman__show_last", &good),
            Some(&json!({}))
        );
        assert!(arguments_for(AgentKind::Grok, "other__show_last", &good).is_none());
        assert_eq!(
            arguments_for(AgentKind::Grok, "askhuman__show_last", &json!({})),
            Some(&json!({}))
        );
        assert!(arguments_for(
            AgentKind::Grok,
            "askhuman__show_last",
            &json!({"tool_name":"askhuman__show_last"})
        )
        .is_none());
        assert_eq!(
            arguments_for(AgentKind::Claude, "mcp__askhuman__show_last", &json!({})),
            Some(&json!({}))
        );
        assert!(
            arguments_for(AgentKind::Cursor, "MCP:show_last", &json!("not an object")).is_none()
        );
    }

    #[test]
    fn claude_binding_never_emits_an_allow_decision() {
        let output = binding_output(AgentKind::Claude, &json!({}), "token".into()).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert!(value
            .pointer("/hookSpecificOutput/permissionDecision")
            .is_none());
        assert_eq!(
            value
                .pointer("/hookSpecificOutput/updatedInput/__askhuman_session_token_v1")
                .and_then(Value::as_str),
            Some("token")
        );
    }

    #[test]
    fn cursor_binding_emits_only_the_patch_token() {
        let output = binding_output(
            AgentKind::Cursor,
            &json!({"message": "preserved by Cursor"}),
            "token".into(),
        )
        .unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(
            value,
            json!({"updated_input": {"__askhuman_session_token_v1": "token"}})
        );
    }

    #[test]
    fn session_id_prefers_hook_input_then_falls_back_to_agent_env() {
        let env = HashMap::from([
            ("CLAUDE_CODE_SESSION_ID".into(), "env-claude".into()),
            ("CURSOR_CONVERSATION_ID".into(), "env-cursor".into()),
        ]);
        assert_eq!(
            session_id_from_hook(
                AgentKind::Claude,
                &json!({"sessionId": "  input-session  "}),
                &env
            )
            .as_deref(),
            Some("input-session")
        );
        assert_eq!(
            session_id_from_hook(AgentKind::Cursor, &json!({}), &env).as_deref(),
            Some("env-cursor")
        );
        assert!(session_id_from_hook(AgentKind::Grok, &json!({}), &env).is_none());
    }

    #[test]
    fn pre_tool_guards_run_before_any_token_side_effect() {
        let env = HashMap::new();
        let valid = json!({
            "tool_name": "mcp__askhuman__ask",
            "tool_input": {"message": "hello"},
            "session_id": "session"
        });
        assert!(
            pre_tool_output_for_mode(AgentKind::Claude, Mode::Cli, Some(&valid), &env).is_none()
        );
        let mut truncated = valid.clone();
        truncated["toolInputTruncated"] = json!(true);
        assert!(
            pre_tool_output_for_mode(AgentKind::Claude, Mode::Mcp, Some(&truncated), &env)
                .is_none()
        );
        let other = json!({
            "tool_name": "mcp__other__ask",
            "tool_input": {"message": "hello"},
            "session_id": "session"
        });
        assert!(
            pre_tool_output_for_mode(AgentKind::Claude, Mode::Mcp, Some(&other), &env).is_none()
        );
        assert!(pre_tool_output_for_mode(AgentKind::Claude, Mode::Mcp, None, &env).is_none());
    }
}
