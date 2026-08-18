//! Hidden Claude Code / Codex PermissionRequest adapter.
//!
//! The raw suggestion ledger stays in this short-lived hook process. The daemon and every surface
//! see only stable action ids; a terminal id can select only a validated suggestion from this run.

use crate::confirm::ActionRole;
use crate::ipc::ConfirmTask;
use crate::models::{
    ConfirmChoice, ConfirmDetail, ConfirmField, ConfirmFieldKind, ConfirmInput,
    ConfirmPresentation, ConfirmResult, ConfirmSpec,
};
use crate::permission_diff::adapters::{normalize_permission_edit, AdapterOutcome};
use serde_json::{json, Map, Value};
use std::io::Read;

const MAX_STDIN_BYTES: u64 = 1024 * 1024;
const MAX_TOOL_INPUT_BYTES: usize = 256 * 1024;
const MAX_BODY_CHARS: usize = 12_000;
const MAX_SUGGESTIONS: usize = 8;
const MAX_RULES: usize = 50;
const MAX_RULE_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Agent {
    Claude,
    Codex,
}

impl Agent {
    fn parse(value: Option<&str>) -> Option<Self> {
        match value? {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }
}

struct ParsedPermission {
    agent: Agent,
    task: ConfirmTask,
    suggestions: Vec<Value>,
}

enum ParseOutcome {
    /// Hook-side auto-allow (every shell segment explicitly rule-allowed, D28/D42):
    /// answer allow immediately without any surface.
    AutoAllow,
    Popup(Box<ParsedPermission>),
}

pub fn run(agent: Option<&str>) -> Option<String> {
    let agent = Agent::parse(agent)?;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return None;
    }
    let input: Value = serde_json::from_slice(&bytes).ok()?;
    let parsed = match parse_permission(agent, &input)? {
        ParseOutcome::AutoAllow => {
            let value = json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" },
                }
            });
            return serde_json::to_string(&value).ok();
        }
        ParseOutcome::Popup(parsed) => parsed,
    };
    let result = crate::client::run_confirm(parsed.task.clone())?;
    decision_output(&parsed, &result).and_then(|value| serde_json::to_string(&value).ok())
}

/// Memory enhancement for Codex requests (spec codex-permission-remember, D24): a fallible
/// layer that may add remember choices + an auto-allow query, or suppress the popup entirely
/// when guardian routing is proven (D36). Claude keeps its native suggestion path.
enum MemoryLayer {
    Suppress,
    Basic,
    AutoAllow,
    Enhanced {
        memory: crate::permission_rules::PermissionMemory,
        extra_choices: Vec<ConfirmChoice>,
    },
}

fn memory_layer(agent: Agent, input: &Value, zh: bool) -> MemoryLayer {
    if agent != Agent::Codex {
        // Claude keeps its native suggestion path; only the built-in AskHuman self-call
        // whitelist (D49) applies, gated on explicit user ask/deny rules.
        if agent == Agent::Claude && crate::permission_memory::claude_self_call(input) {
            return MemoryLayer::AutoAllow;
        }
        return MemoryLayer::Basic;
    }
    match crate::permission_memory::analyze_codex(input, zh) {
        crate::permission_memory::Analysis::Suppress => MemoryLayer::Suppress,
        crate::permission_memory::Analysis::Basic => MemoryLayer::Basic,
        crate::permission_memory::Analysis::AutoAllow => MemoryLayer::AutoAllow,
        crate::permission_memory::Analysis::Enhanced {
            memory,
            extra_choices,
        } => MemoryLayer::Enhanced {
            memory,
            extra_choices,
        },
    }
}

fn parse_permission(agent: Agent, input: &Value) -> Option<ParseOutcome> {
    let object = input.as_object()?;
    if object.get("hook_event_name").and_then(Value::as_str) != Some("PermissionRequest") {
        return None;
    }
    let session_id = required_string(object, "session_id", 256)?;
    let cwd = required_string(object, "cwd", 8_192)?;
    let permission_mode = required_string(object, "permission_mode", 128)?;
    let tool_name = required_string(object, "tool_name", 512)?;
    // Claude's built-in question tool asks the human something; it never touches their machine, so
    // an approval card is pure noise (spec claude-ask-user-question D2). Stay out of the way
    // entirely — a bare `allow` does not satisfy the tool's interaction requirement, and the
    // takeover, when enabled, happens earlier in PreToolUse.
    if agent == Agent::Claude && tool_name == "AskUserQuestion" {
        return None;
    }
    let tool_input = object.get("tool_input")?.clone();
    if serde_json::to_vec(&tool_input).ok()?.len() > MAX_TOOL_INPUT_BYTES {
        return None;
    }

    let (summary, body) = summarize_tool(&tool_name, &tool_input);
    let popup_edit = match normalize_permission_edit(agent.id(), &tool_name, &tool_input, &cwd) {
        AdapterOutcome::Intent(intent) => Some(intent),
        AdapterOutcome::NotNativeEdit => None,
    };
    let now = crate::history::now_ms();
    let project_name = crate::project::display_name(&cwd);
    let lang = crate::i18n::Lang::current();
    let zh = lang == crate::i18n::Lang::Zh;
    let field = |id: &str, en: &str, zh_label: &str, value: String, kind| ConfirmField {
        id: id.to_string(),
        label: if zh { zh_label } else { en }.to_string(),
        value,
        kind,
    };
    let context = vec![
        field(
            "agent",
            "Agent",
            "Agent",
            agent.label().into(),
            ConfirmFieldKind::Text,
        ),
        field(
            "project",
            "Project",
            "项目",
            project_name,
            ConfirmFieldKind::Text,
        ),
        field(
            "workspace",
            "Workspace",
            "工作区",
            cwd.clone(),
            ConfirmFieldKind::Path,
        ),
        field(
            "tool",
            "Tool",
            "工具",
            tool_name.clone(),
            ConfirmFieldKind::Text,
        ),
        field(
            "permission_mode",
            "Permission mode",
            "权限模式",
            permission_mode,
            ConfirmFieldKind::Text,
        ),
        field(
            "created_at",
            "Requested at",
            "请求时间",
            now.to_string(),
            ConfirmFieldKind::Timestamp,
        ),
    ];

    let memory_layer = memory_layer(agent, input, zh);
    if matches!(memory_layer, MemoryLayer::Suppress) {
        // Provably guardian-routed (D36): no popup, no decision.
        return None;
    }
    if matches!(memory_layer, MemoryLayer::AutoAllow) {
        return Some(ParseOutcome::AutoAllow);
    }

    let mut choices = vec![ConfirmChoice {
        id: "approve_once".into(),
        label: if zh { "允许" } else { "Allow" }.into(),
        description: String::new(),
        role: ActionRole::Primary,
        variant: None,
    }];
    let mut suggestions = Vec::new();
    let mut omitted = 0usize;
    if agent == Agent::Claude {
        for suggestion in object
            .get("permission_suggestions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(rendered) = validate_suggestion(suggestion, zh) else {
                continue;
            };
            if suggestions.len() >= MAX_SUGGESTIONS {
                omitted += 1;
                continue;
            }
            let index = suggestions.len();
            suggestions.push(suggestion.clone());
            let mut lines = rendered.lines();
            let label = lines.next().unwrap_or_default().to_string();
            let description = lines.collect::<Vec<_>>().join("\n");
            choices.push(ConfirmChoice {
                id: format!("permission_suggestion_{index}"),
                label,
                description,
                role: ActionRole::Default,
                variant: None,
            });
        }
    }
    let mut memory = match memory_layer {
        MemoryLayer::Enhanced {
            memory,
            extra_choices,
        } => {
            choices.extend(extra_choices);
            Some(memory)
        }
        _ => None,
    };
    // YOLO session opt-in (D53): every Codex permission popup — dangerous commands and
    // Basic (no memory enhancement) popups included — carries the choice. Once saved,
    // the daemon auto-allows all further requests of this session, so an active grant
    // never re-renders this choice. Off-switches: settings panel / IM `/yolo`.
    if agent == Agent::Codex {
        choices.push(ConfirmChoice {
            id: crate::permission_memory::ACTION_YOLO.into(),
            label: if zh {
                "本对话开启 YOLO 模式：自动允许一切".into()
            } else {
                "YOLO mode this conversation: auto-allow everything".into()
            },
            description: if zh {
                "本对话所有权限请求（含危险命令）自动放行 · 设置面板或 IM 发 /yolo 可关闭".into()
            } else {
                "Every permission request of this conversation (dangerous commands included) \
                 auto-allows · turn off in settings or via /yolo in IM"
                    .into()
            },
            role: ActionRole::Destructive,
            variant: None,
        });
        let save = crate::permission_rules::MemorySave {
            action_id: crate::permission_memory::ACTION_YOLO.into(),
            namespace: crate::permission_rules::RuleNamespace::Session,
            rules: vec![crate::permission_rules::RuleKey::Yolo],
            native: None,
        };
        match memory.as_mut() {
            Some(memory) => memory.saves.push(save),
            None => {
                memory = Some(crate::permission_rules::PermissionMemory {
                    query: None,
                    saves: vec![save],
                });
            }
        }
    }
    choices.push(ConfirmChoice {
        id: "deny".into(),
        label: if zh { "拒绝" } else { "Deny" }.into(),
        description: String::new(),
        role: ActionRole::Destructive,
        variant: None,
    });

    let mut body_md = body;
    if omitted > 0 {
        body_md.push_str(&format!(
            "\n\n> {}",
            if zh {
                format!("另有 {omitted} 条合法权限建议未展示")
            } else {
                format!("{omitted} additional valid permission suggestion(s) not shown")
            }
        ));
    }
    let spec = ConfirmSpec {
        title: if zh {
            "Agent 请求调用以下工具"
        } else {
            "Agent requests to use the following tool"
        }
        .into(),
        context,
        detail: ConfirmDetail { summary, body_md },
        choices,
        presentation: ConfirmPresentation::SingleSelectSubmit {
            input: Some(ConfirmInput {
                id: "reason".into(),
                visible_when_action_id: "deny".into(),
                always_visible: false,
                required: false,
                prefix_chars_by_action_id: Default::default(),
                label: if zh {
                    "拒绝原因（可选）"
                } else {
                    "Reason for denial (optional)"
                }
                .into(),
                placeholder: if zh {
                    "告诉 Agent 应该怎么做"
                } else {
                    "Tell the Agent what it should do"
                }
                .into(),
                max_chars: 1000,
            }),
            submit_label: if zh {
                "提交决定"
            } else {
                "Submit decision"
            }
            .into(),
            default_action_id: None,
        },
        dismiss_action_id: "deny".into(),
    };
    Some(ParseOutcome::Popup(Box::new(ParsedPermission {
        agent,
        task: ConfirmTask {
            spec,
            popup_edit,
            source: agent.label().into(),
            lang: if zh { "zh" } else { "en" }.into(),
            project: cwd,
            agent_kind: agent.id().into(),
            agent_session_id: session_id,
            caller_pid: std::process::id(),
            memory,
        },
        suggestions,
    })))
}

fn required_string(object: &Map<String, Value>, key: &str, max: usize) -> Option<String> {
    let value = object.get(key)?.as_str()?.trim();
    (!value.is_empty() && value.chars().count() <= max).then(|| value.to_string())
}

fn summarize_tool(tool: &str, input: &Value) -> (String, String) {
    let summary = input
        .get("description")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match tool {
            "Bash" => "Run a shell command".into(),
            "apply_patch" | "Edit" | "Write" | "NotebookEdit" => "Modify files".into(),
            value if value.starts_with("mcp__") => "Call an MCP tool".into(),
            _ => format!("Use {tool}"),
        });
    let raw = if let Some(command) = input.get("command").and_then(Value::as_str) {
        format!(
            "```sh\n{}\n```",
            safe_fence(command, MAX_BODY_CHARS.saturating_sub(10))
        )
    } else if let Some(path) = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
    {
        format!(
            "**Path:** `{}`\n\n```json\n{}\n```",
            path.replace('`', "\\`"),
            pretty_json(input)
        )
    } else {
        format!("```json\n{}\n```", pretty_json(input))
    };
    (summary, truncate_chars(&raw, MAX_BODY_CHARS))
}

fn pretty_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| "null".into())
}

fn safe_fence(value: &str, max: usize) -> String {
    truncate_chars(value, max).replace("```", "``\u{200b}`")
}

fn truncate_chars(value: &str, max: usize) -> String {
    let mut chars = value.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}\n… [truncated]")
    } else {
        head
    }
}

fn validate_suggestion(value: &Value, zh: bool) -> Option<String> {
    let object = value.as_object()?;
    if object
        .keys()
        .any(|key| !matches!(key.as_str(), "type" | "rules" | "behavior" | "destination"))
        || object.get("type").and_then(Value::as_str) != Some("addRules")
        || object.get("behavior").and_then(Value::as_str) != Some("allow")
    {
        return None;
    }
    let destination = object.get("destination")?.as_str()?;
    if !matches!(
        destination,
        "session" | "localSettings" | "projectSettings" | "userSettings"
    ) {
        return None;
    }
    let rules = object.get("rules")?.as_array()?;
    if rules.is_empty() || rules.len() > MAX_RULES {
        return None;
    }
    let mut parsed_rules = Vec::new();
    for rule in rules {
        let rule = rule.as_object()?;
        if rule
            .keys()
            .any(|key| !matches!(key.as_str(), "toolName" | "ruleContent"))
        {
            return None;
        }
        let tool = required_string(rule, "toolName", 256)?;
        let content = match rule.get("ruleContent") {
            Some(Value::String(value))
                if !value.trim().is_empty() && value.chars().count() <= MAX_RULE_CHARS =>
            {
                Some(value.split_whitespace().collect::<Vec<_>>().join(" "))
            }
            None => None,
            _ => return None,
        };
        parsed_rules.push((tool, content));
    }
    let scope = match (zh, destination) {
        (true, "session") => "本会话允许",
        (true, "localSettings") => "始终允许（Local）",
        (true, "projectSettings") => "始终允许（Project）",
        (true, "userSettings") => "始终允许（User）",
        (false, "session") => "Allow in this session",
        (false, "localSettings") => "Always allow (Local)",
        (false, "projectSettings") => "Always allow (Project)",
        (false, "userSettings") => "Always allow (User)",
        _ => return None,
    };
    if parsed_rules.len() == 1 {
        let (tool, content) = &parsed_rules[0];
        let target = content.clone().unwrap_or_else(|| {
            if zh {
                format!("整个 {tool} 工具")
            } else {
                format!("the entire {tool} tool")
            }
        });
        return Some(if zh {
            format!("{scope}：{target}")
        } else {
            format!("{scope}: {target}")
        });
    }
    let count = parsed_rules.len();
    let mut description = if zh {
        format!("{scope}：{count} 条规则")
    } else {
        format!("{scope}: {count} rules")
    };
    for (tool, content) in parsed_rules {
        let target = content.unwrap_or_else(|| {
            if zh {
                "整个工具".to_string()
            } else {
                "entire tool".to_string()
            }
        });
        if zh {
            description.push_str(&format!("\n{tool}：{target}"));
        } else {
            description.push_str(&format!("\n{tool}: {target}"));
        }
    }
    Some(description)
}

fn decision_output(parsed: &ParsedPermission, result: &ConfirmResult) -> Option<Value> {
    let mut decision = Map::new();
    match result.action_id.as_str() {
        "approve_once" => {
            decision.insert("behavior".into(), json!("allow"));
        }
        // Daemon answered from stored shadow rules without any surface: memory auto-allow
        // (query hit) or a session-wide mode (relaxed D52 / yolo D53, the latter needing
        // no query — any task carrying memory metadata may be answered this way).
        crate::permission_rules::AUTO_ALLOW_ACTION_ID if parsed.task.memory.is_some() => {
            decision.insert("behavior".into(), json!("allow"));
        }
        // A remember choice: the daemon has already persisted (or degraded) the rules; the
        // current call maps to a plain allow either way (D25/D26).
        action
            if parsed
                .task
                .memory
                .as_ref()
                .is_some_and(|memory| memory.saves.iter().any(|save| save.action_id == action)) =>
        {
            decision.insert("behavior".into(), json!("allow"));
        }
        "deny" => {
            decision.insert("behavior".into(), json!("deny"));
            let comment = result
                .comment
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            let message = match comment {
                Some(comment) => format!(
                    "The user denied this permission request via AskHuman. Reason: {comment}"
                ),
                None => "The user denied this permission request via AskHuman.".to_string(),
            };
            decision.insert("message".into(), json!(message));
        }
        action if parsed.agent == Agent::Claude => {
            let index = action
                .strip_prefix("permission_suggestion_")?
                .parse::<usize>()
                .ok()?;
            let suggestion = parsed.suggestions.get(index)?.clone();
            decision.insert("behavior".into(), json!("allow"));
            decision.insert("updatedPermissions".into(), Value::Array(vec![suggestion]));
        }
        _ => return None,
    }
    Some(json!({
        "hookSpecificOutput": {
            "hookEventName": "PermissionRequest",
            "decision": Value::Object(decision),
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(agent: Agent) -> Value {
        let mut value = json!({
            "session_id": "s1",
            "cwd": "/tmp/project",
            "permission_mode": "default",
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "git status", "description": "Inspect repository" },
        });
        if agent == Agent::Claude {
            value["permission_suggestions"] = json!([{
                "type": "addRules",
                "rules": [{ "toolName": "Bash", "ruleContent": "git status" }],
                "behavior": "allow",
                "destination": "session"
            }]);
        }
        value
    }

    fn result(action: &str, comment: Option<&str>) -> ConfirmResult {
        ConfirmResult {
            action_id: action.into(),
            comment: comment.map(str::to_string),
            source_channel_id: "popup".into(),
        }
    }

    /// Test view of parse_permission for the popup path.
    #[test]
    fn claudes_own_question_tool_never_becomes_an_approval_card() {
        let mut value = input(Agent::Claude);
        value["tool_name"] = json!("AskUserQuestion");
        value["tool_input"] = json!({
            "questions": [{ "question": "Which framework?", "options": [{ "label": "React" }] }]
        });
        // Not even an auto-allow: we stay out of the way entirely (spec D2).
        assert!(parse_permission(Agent::Claude, &value).is_none());
    }

    fn parse_popup(agent: Agent, input: &Value) -> Option<ParsedPermission> {
        match parse_permission(agent, input)? {
            ParseOutcome::Popup(parsed) => Some(*parsed),
            ParseOutcome::AutoAllow => None,
        }
    }

    #[test]
    fn claude_suggestion_is_replayed_from_private_ledger() {
        let parsed = parse_popup(Agent::Claude, &input(Agent::Claude)).unwrap();
        assert!(matches!(
            parsed.task.spec.choices[0].label.as_str(),
            "允许" | "Allow"
        ));
        assert!(!matches!(
            parsed.task.spec.choices[1].label.as_str(),
            "更新权限并批准" | "Update permission and approve"
        ));
        assert!(parsed.task.spec.choices[1].label.contains("git status"));
        assert!(parsed.task.spec.choices[1].description.is_empty());
        let output = decision_output(&parsed, &result("permission_suggestion_0", None)).unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        assert_eq!(
            output["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["destination"],
            "session"
        );
    }

    #[test]
    fn codex_never_accepts_a_permission_update_action() {
        let parsed = parse_popup(Agent::Codex, &input(Agent::Codex)).unwrap();
        assert!(decision_output(&parsed, &result("permission_suggestion_0", None)).is_none());
        let deny = decision_output(&parsed, &result("deny", Some("unsafe"))).unwrap();
        assert_eq!(
            deny["hookSpecificOutput"]["decision"]["message"],
            "The user denied this permission request via AskHuman. Reason: unsafe"
        );
    }

    #[test]
    fn native_edit_intent_is_popup_only_metadata() {
        let mut value = input(Agent::Claude);
        value["tool_name"] = json!("Edit");
        value["tool_input"] = json!({
            "file_path": "/tmp/project/a.txt",
            "old_string": "old",
            "new_string": "new"
        });
        let parsed = parse_popup(Agent::Claude, &value).unwrap();
        let intent = parsed.task.popup_edit.as_ref().unwrap();
        assert_eq!(intent.native_tool, "Edit");
        assert_eq!(intent.workspace, "/tmp/project");
        assert_eq!(
            intent.initial_diff.as_ref().unwrap().snapshot_status,
            crate::permission_diff::SnapshotStatus::PayloadOnly
        );
    }

    #[test]
    fn malformed_or_non_allow_suggestions_are_ignored() {
        let mut input = input(Agent::Claude);
        input["permission_suggestions"] = json!([
            { "type": "setMode", "behavior": "allow", "destination": "session" },
            { "type": "addRules", "rules": [], "behavior": "allow", "destination": "session" },
            { "type": "addRules", "rules": [{"toolName":"Bash"}], "behavior": "deny", "destination": "session" }
        ]);
        let parsed = parse_popup(Agent::Claude, &input).unwrap();
        assert!(parsed.suggestions.is_empty());
        assert_eq!(parsed.task.spec.choices.len(), 2);
    }

    #[test]
    fn suggestion_descriptions_are_compact_and_keep_multi_rule_details() {
        let single = json!({
            "type": "addRules",
            "rules": [{ "toolName": "Bash", "ruleContent": "git   status" }],
            "behavior": "allow",
            "destination": "localSettings",
        });
        assert_eq!(
            validate_suggestion(&single, true).as_deref(),
            Some("始终允许（Local）：git status")
        );
        let whole_tool = json!({
            "type": "addRules",
            "rules": [{ "toolName": "Read" }],
            "behavior": "allow",
            "destination": "userSettings",
        });
        assert_eq!(
            validate_suggestion(&whole_tool, true).as_deref(),
            Some("始终允许（User）：整个 Read 工具")
        );
        let multiple = json!({
            "type": "addRules",
            "rules": [
                { "toolName": "Bash", "ruleContent": "git status" },
                { "toolName": "Read" }
            ],
            "behavior": "allow",
            "destination": "projectSettings",
        });
        assert_eq!(
            validate_suggestion(&multiple, true).as_deref(),
            Some("始终允许（Project）：2 条规则\nBash：git status\nRead：整个工具")
        );
    }

    #[test]
    fn context_and_default_selection_follow_permission_contract() {
        let parsed = parse_popup(Agent::Codex, &input(Agent::Codex)).unwrap();
        let ids: Vec<&str> = parsed
            .task
            .spec
            .context
            .iter()
            .map(|field| field.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "agent",
                "project",
                "workspace",
                "tool",
                "permission_mode",
                "created_at"
            ]
        );
        assert_eq!(parsed.task.spec.presentation.default_action_id(), None);
    }

    #[test]
    fn approve_discards_comment_and_unknown_action_fails_closed() {
        let parsed = parse_popup(Agent::Claude, &input(Agent::Claude)).unwrap();
        let allow =
            decision_output(&parsed, &result("approve_once", Some("inject\njson"))).unwrap();
        assert!(allow["hookSpecificOutput"]["decision"]
            .get("message")
            .is_none());
        assert!(decision_output(&parsed, &result("permission_suggestion_99", None)).is_none());
        assert!(decision_output(&parsed, &result("approve_once\"}", None)).is_none());
    }

    #[test]
    fn suggestions_are_bounded_and_keep_original_objects_private() {
        let mut value = input(Agent::Claude);
        value["permission_suggestions"] = Value::Array(
            (0..10)
                .map(|index| {
                    json!({
                        "type": "addRules",
                        "rules": [{ "toolName": "Bash", "ruleContent": format!("echo {index}") }],
                        "behavior": "allow",
                        "destination": "session",
                    })
                })
                .collect(),
        );
        let parsed = parse_popup(Agent::Claude, &value).unwrap();
        assert_eq!(parsed.suggestions.len(), MAX_SUGGESTIONS);
        assert_eq!(parsed.task.spec.choices.len(), MAX_SUGGESTIONS + 2);
        assert!(parsed.task.spec.detail.body_md.contains('2'));
        let output = decision_output(
            &parsed,
            &result(
                &format!("permission_suggestion_{}", MAX_SUGGESTIONS - 1),
                None,
            ),
        )
        .unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["decision"]["updatedPermissions"][0]["rules"][0]
                ["ruleContent"],
            format!("echo {}", MAX_SUGGESTIONS - 1)
        );
    }

    #[test]
    fn codex_popups_carry_yolo_choice_and_claude_does_not() {
        // Codex (Basic layer here: no transcript → no memory enhancement) still gets the
        // YOLO opt-in right before deny, with a session Yolo save behind it (D53).
        let parsed = parse_popup(Agent::Codex, &input(Agent::Codex)).unwrap();
        let choices = &parsed.task.spec.choices;
        assert_eq!(
            choices[choices.len() - 2].id,
            crate::permission_memory::ACTION_YOLO
        );
        assert_eq!(choices.last().unwrap().id, "deny");
        let memory = parsed.task.memory.as_ref().unwrap();
        assert!(memory.query.is_none());
        let save = memory
            .saves
            .iter()
            .find(|save| save.action_id == crate::permission_memory::ACTION_YOLO)
            .unwrap();
        assert_eq!(save.rules, vec![crate::permission_rules::RuleKey::Yolo]);
        assert!(save.native.is_none());
        // Choosing it maps to allow (rules persist daemon-side, D25/D26).
        let output = decision_output(
            &parsed,
            &result(crate::permission_memory::ACTION_YOLO, None),
        )
        .unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        // Claude keeps its native path: no YOLO choice, no memory.
        let claude = parse_popup(Agent::Claude, &input(Agent::Claude)).unwrap();
        assert!(claude
            .task
            .spec
            .choices
            .iter()
            .all(|choice| choice.id != crate::permission_memory::ACTION_YOLO));
        assert!(claude.task.memory.is_none());
    }

    #[test]
    fn auto_allow_action_is_accepted_without_a_query() {
        // YOLO auto-allow (D53) answers tasks whose memory carries no query (e.g. dangerous
        // command popups); the hook must map it to allow.
        let parsed = parse_popup(Agent::Codex, &input(Agent::Codex)).unwrap();
        assert!(parsed
            .task
            .memory
            .as_ref()
            .is_some_and(|memory| memory.query.is_none()));
        let output = decision_output(
            &parsed,
            &result(crate::permission_rules::AUTO_ALLOW_ACTION_ID, None),
        )
        .unwrap();
        assert_eq!(
            output["hookSpecificOutput"]["decision"]["behavior"],
            "allow"
        );
        // Claude tasks carry no memory: the auto-allow action stays rejected.
        let claude = parse_popup(Agent::Claude, &input(Agent::Claude)).unwrap();
        assert!(decision_output(
            &claude,
            &result(crate::permission_rules::AUTO_ALLOW_ACTION_ID, None)
        )
        .is_none());
    }

    #[test]
    fn deny_reason_is_json_escaped_under_fixed_prefix() {
        let parsed = parse_popup(Agent::Codex, &input(Agent::Codex)).unwrap();
        let output = decision_output(&parsed, &result("deny", Some("line 1\n\"line 2\""))).unwrap();
        let encoded = serde_json::to_string(&output).unwrap();
        let decoded: Value = serde_json::from_str(&encoded).unwrap();
        assert_eq!(
            decoded["hookSpecificOutput"]["decision"]["message"],
            "The user denied this permission request via AskHuman. Reason: line 1\n\"line 2\""
        );
    }
}
