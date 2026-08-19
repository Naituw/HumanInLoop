//! Stop confirmation hook. It reuses the ordinary single-select Ask flow and emits each agent's
//! native continuation JSON from the human's decision.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::{AgentKind, LifecycleEvent};
use crate::i18n::Lang;
use crate::ipc::TaskRequest;
use crate::models::{MessagePrompt, OptionItem, OutputFormat, Question};

const MAX_LAST_MESSAGE_CHARS: usize = 2_000;
const MAX_INSTRUCTION_CHARS: usize = 1_000;
const HOOK_WAIT_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq, Eq)]
enum StopDecision {
    End,
    Continue(Option<String>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LastAssistantMessage {
    display: Option<String>,
    user_confirmed_end_turn: bool,
}

/// Entry point for `AskHuman __stop-hook <agent> [track] [confirm]`.
/// Stdout always contains exactly one JSON object.
pub fn run(args: &[String]) {
    let output = run_inner(args).unwrap_or_else(|| json!({}));
    println!("{output}");
}

fn run_inner(args: &[String]) -> Option<Value> {
    let kind = args.first().and_then(|value| AgentKind::parse(value))?;
    if kind == AgentKind::Grok {
        return None;
    }
    let track = args.get(1).is_some_and(|value| value == "track");
    let confirm = args.iter().skip(1).any(|value| value == "confirm");
    let env: HashMap<String, String> = std::env::vars().collect();
    if super::report::should_skip(kind, &env) {
        return None;
    }
    let input = super::report::read_stdin_json()?;
    let session_id = super::report::resolve_session_id(kind, &env, Some(&input));
    if session_id.is_empty() {
        return None;
    }
    let cwd = super::report::resolve_cwd(&env, Some(&input));

    // Cursor also emits stop for aborted/error turns, but only consumes followup_message for a
    // completed turn. Non-natural stops only retain the existing lifecycle turn-end behavior.
    if !is_natural_stop(kind, &input) {
        if track {
            super::report::report_simple_event(kind, LifecycleEvent::TurnEnd, session_id, cwd);
        }
        return None;
    }

    if !confirm {
        if track {
            super::report::report_simple_event(kind, LifecycleEvent::TurnEnd, session_id, cwd);
        }
        return None;
    }
    if let Some(reason) = confirmation_suppression_reason(kind, &input) {
        crate::daemon::lifecycle::log_guard_audit(crate::daemon::lifecycle::GuardAudit {
            component: "stop_confirmation",
            action: "suppressed",
            reason,
            tool: None,
            agent: Some(kind.as_str()),
            thread_source: codex_thread_source(&input),
            session_id: Some(&session_id),
            thread_id: input
                .get("thread_id")
                .or_else(|| input.get("threadId"))
                .and_then(Value::as_str),
            turn_id: input
                .get("turn_id")
                .or_else(|| input.get("turnId"))
                .and_then(Value::as_str),
        });
        if track {
            super::report::report_simple_event(kind, LifecycleEvent::TurnEnd, session_id, cwd);
        }
        return None;
    }

    let last_message = last_assistant_message(kind, &input);
    if last_message.user_confirmed_end_turn {
        if track {
            super::report::report_simple_event(kind, LifecycleEvent::TurnEnd, session_id, cwd);
        }
        return None;
    }
    let lang = Lang::current();
    // Todo dispatch on the Stop card (spec todo-whats-next D5): the hook's cwd maps to the git
    // root, whose pending todos become leading options. Dequeue happens centrally in the
    // Coordinator (options carry todo_id), not here. Only the first MAX_OPTION_TODOS entries
    // become options (channel option-count limits, round 14); the overflow note goes into the
    // question text. `parse_ask_decision` gets the same truncated list (index math stays aligned).
    let project = cwd
        .as_deref()
        .map(|c| crate::project::detect_from(Path::new(c)))
        .unwrap_or_default();
    let mut todos = if project.is_empty() {
        Vec::new()
    } else {
        crate::todos::list(&project)
    };
    let total_todos = todos.len();
    todos.truncate(crate::todos::MAX_OPTION_TODOS);
    let task = build_task(
        kind,
        &session_id,
        &project,
        &todos,
        total_todos,
        last_message.display.as_deref(),
        lang,
    );
    let captured = crate::client::run_ask_capture(task, Duration::from_secs(HOOK_WAIT_SECS));
    let decision = captured
        .as_deref()
        .map(|stdout| parse_ask_decision(stdout, &todos))
        .unwrap_or(StopDecision::End);

    match decision {
        StopDecision::Continue(instruction) => Some(continuation_output(
            kind,
            &crate::prompts::stop_continue_prompt(kind, instruction.as_deref()),
        )),
        StopDecision::End => {
            if track {
                super::report::report_simple_event(kind, LifecycleEvent::TurnEnd, session_id, cwd);
            }
            None
        }
    }
}

fn is_natural_stop(kind: AgentKind, input: &Value) -> bool {
    match kind {
        AgentKind::Cursor => input.get("status").and_then(Value::as_str) == Some("completed"),
        AgentKind::Claude | AgentKind::Codex => input
            .get("hook_event_name")
            .or_else(|| input.get("hookEventName"))
            .and_then(Value::as_str)
            .is_some_and(|event| event.eq_ignore_ascii_case("stop")),
        AgentKind::Grok => false,
        AgentKind::Pi => {
            input.get("stop_reason").and_then(Value::as_str) == Some("stop")
                && input
                    .get("hook_event_name")
                    .and_then(Value::as_str)
                    .is_some_and(|event| event.eq_ignore_ascii_case("stop"))
        }
    }
}

fn codex_thread_source(input: &Value) -> Option<&str> {
    input
        .get("thread_source")
        .or_else(|| input.get("threadSource"))
        .and_then(Value::as_str)
}

fn confirmation_suppression_reason(kind: AgentKind, input: &Value) -> Option<&'static str> {
    if kind != AgentKind::Codex {
        return None;
    }
    if codex_thread_source(input)
        .is_some_and(|source| crate::mcp::ask::CODEX_BLOCKED_THREAD_SOURCES.contains(&source))
    {
        return Some("codex_blocked_thread_source");
    }
    input
        .get("transcript_path")
        .or_else(|| input.get("transcriptPath"))
        .is_some_and(Value::is_null)
        .then_some("codex_transcript_path_null")
}

fn last_assistant_message(kind: AgentKind, input: &Value) -> LastAssistantMessage {
    let raw = match kind {
        AgentKind::Claude | AgentKind::Codex => input
            .get("last_assistant_message")
            .or_else(|| input.get("lastAssistantMessage"))
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string),
        AgentKind::Cursor => cursor_last_message(input),
        AgentKind::Grok => None,
        AgentKind::Pi => input
            .get("last_assistant_message")
            .or_else(|| input.get("lastAssistantMessage"))
            .and_then(Value::as_str)
            .filter(|text| !text.trim().is_empty())
            .map(str::to_string),
    };
    normalize_last_assistant_message(raw.as_deref())
}

fn normalize_last_assistant_message(raw: Option<&str>) -> LastAssistantMessage {
    let Some(raw) = raw else {
        return LastAssistantMessage {
            display: None,
            user_confirmed_end_turn: false,
        };
    };
    let (cleaned, user_confirmed_end_turn) = strip_confirmed_end_turn_marker(raw);
    LastAssistantMessage {
        display: (!cleaned.trim().is_empty())
            .then(|| truncate_preserving_layout(&cleaned, MAX_LAST_MESSAGE_CHARS)),
        user_confirmed_end_turn,
    }
}

fn cursor_last_message(input: &Value) -> Option<String> {
    cursor_last_message_under(input, &crate::paths::cursor_dir())
}

fn cursor_last_message_under(input: &Value, root: &Path) -> Option<String> {
    let raw = input.get("transcript_path")?.as_str()?.trim();
    if raw.is_empty() || raw.chars().count() > 8_192 {
        return None;
    }
    let path = canonical_under(Path::new(raw), root)?;
    super::activity::resolve_last_assistant_text_from_path_raw(AgentKind::Cursor, &path)
}

/// Detect the end-turn marker anywhere in the last assistant message.
///
/// The prompt still asks agents to put it on a final independent line, but in practice they wrap
/// it in markdown, glue punctuation, or write more text after it. Any occurrence of the exact
/// marker substring is treated as user-confirmed end so the Stop card is not shown again.
fn strip_confirmed_end_turn_marker(text: &str) -> (String, bool) {
    let marker = crate::prompts::USER_CONFIRMED_END_TURN_MARKER;
    if !text.contains(marker) {
        return (text.to_string(), false);
    }
    // Remove every occurrence; trim edges left by marker-only lines.
    let cleaned = text.replace(marker, "").trim().to_string();
    (cleaned, true)
}

fn canonical_under(path: &Path, root: &Path) -> Option<PathBuf> {
    let path = std::fs::canonicalize(path).ok()?;
    let root = std::fs::canonicalize(root).ok()?;
    (path.is_file() && path.starts_with(root)).then_some(path)
}

fn truncate_preserving_layout(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_chars).collect();
        format!("{}\n\n… [truncated]", head.trim_end())
    }
}

fn build_task(
    kind: AgentKind,
    session_id: &str,
    project: &str,
    todos: &[crate::todos::TodoEntry],
    total_todos: usize,
    last_message: Option<&str>,
    lang: Lang,
) -> TaskRequest {
    let (question, continue_label, end_label, unavailable) = match lang {
        Lang::Zh => (
            "Agent 准备结束本轮对话，接下来怎么做？",
            "继续对话",
            "结束对话",
            "未能读取 Agent 的最后一段回复。",
        ),
        Lang::En => (
            "The Agent is ready to end this turn. What should happen next?",
            "Continue conversation",
            "End conversation",
            "The Agent's last response could not be read.",
        ),
    };
    let message = last_message.unwrap_or(unavailable).to_string();
    // Options: todo chips first (spec D5), then the two original actions. Index math in
    // `parse_ask_decision` relies on this order. Labels carry the "Run todo: " display prefix
    // (same as whats-next); the continuation text comes from the raw entry via index, not the label.
    let mut options: Vec<OptionItem> = todos
        .iter()
        .map(|entry| OptionItem::with_todo_entry(crate::todos::option_label(lang, entry), entry))
        .collect();
    options.push(OptionItem::new(continue_label, true));
    options.push(OptionItem::new(end_label, false));
    // Overflow note (round 14): appended to the question text when the queue exceeds the cap.
    let mut question = question.to_string();
    if let Some(note) = crate::todos::overflow_note(total_todos, lang) {
        question.push_str("\n\n");
        question.push_str(&note);
    }
    TaskRequest {
        message: MessagePrompt::new(message, Vec::new()),
        questions: vec![Question::new(question, options)],
        is_markdown: true,
        source: crate::models::source_name_for_agent(Some(kind)),
        lang: lang.code().to_string(),
        project: project.to_string(),
        select_only: false,
        single: true,
        output_format: OutputFormat::Json,
        record_history: false,
        agent_kind: Some(kind.as_str().to_string()),
        agent_session_id: Some(session_id.to_string()),
        mcp_instance_id: None,
        agent_pid: None,
        caller_pid: std::process::id(),
        from_mcp: false,
        perf_id: String::new(),
        perf_autodismiss: false,
        whats_next: false,
    }
}

/// Map the Ask JSON to a decision. Option order is fixed by `build_task`: todo chips occupy
/// indices `0..todos.len()`, then "continue" and "end" (spec D5 mapping):
/// end = end (text discarded) · todo (± text) = continue with that todo (+ supplement) ·
/// continue / free text only = original semantics. Dequeue is not done here (Coordinator does it).
fn parse_ask_decision(stdout: &str, todos: &[crate::todos::TodoEntry]) -> StopDecision {
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return StopDecision::End;
    };
    if value.get("action").and_then(Value::as_str) != Some("answer") {
        return StopDecision::End;
    }
    let Some(answer) = value
        .get("answers")
        .and_then(Value::as_array)
        .and_then(|answers| answers.first())
    else {
        return StopDecision::End;
    };
    let continue_index = todos.len() as u64;
    let end_index = continue_index + 1;
    let indices = answer
        .get("selected_indices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if indices
        .iter()
        .any(|index| index.as_u64() == Some(end_index))
    {
        return StopDecision::End;
    }
    let instruction = answer
        .get("user_input")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(|text| truncate_preserving_layout(text, MAX_INSTRUCTION_CHARS));
    let files: Vec<String> = answer
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if let Some(todo) = indices
        .iter()
        .filter_map(Value::as_u64)
        .find(|index| *index < continue_index)
        .and_then(|index| todos.get(index as usize))
    {
        // Todo text is always delivered in full (spec D11); the typed supplement follows after a
        // blank line, same as whats-next.
        let prompt = match &instruction {
            Some(extra) => format!("{}\n\n{}", todo.text, extra),
            None => todo.text.clone(),
        };
        return StopDecision::Continue(Some(
            crate::integrations::agent_launch::task_with_attachments(&prompt, &files, &[]),
        ));
    }
    if indices
        .iter()
        .any(|index| index.as_u64() == Some(continue_index))
        || instruction.is_some()
    {
        StopDecision::Continue(instruction)
    } else {
        StopDecision::End
    }
}

fn continuation_output(kind: AgentKind, prompt: &str) -> Value {
    match kind {
        AgentKind::Cursor => json!({ "followup_message": prompt }),
        AgentKind::Claude | AgentKind::Codex => {
            json!({ "decision": "block", "reason": prompt })
        }
        AgentKind::Grok => json!({}),
        AgentKind::Pi => json!({ "followup_message": prompt }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_stop_matrix() {
        assert!(is_natural_stop(
            AgentKind::Claude,
            &json!({"hook_event_name":"Stop"})
        ));
        assert!(is_natural_stop(
            AgentKind::Codex,
            &json!({"hook_event_name":"Stop"})
        ));
        assert!(is_natural_stop(
            AgentKind::Cursor,
            &json!({"status":"completed"})
        ));
        assert!(!is_natural_stop(
            AgentKind::Cursor,
            &json!({"status":"error"})
        ));
        assert!(!is_natural_stop(
            AgentKind::Cursor,
            &json!({"status":"aborted"})
        ));
        assert!(!is_natural_stop(
            AgentKind::Claude,
            &json!({"hook_event_name":"StopFailure"})
        ));
        assert!(!is_natural_stop(AgentKind::Codex, &json!({})));
        assert!(is_natural_stop(
            AgentKind::Pi,
            &json!({"hook_event_name":"Stop","stop_reason":"stop"})
        ));
        assert!(!is_natural_stop(
            AgentKind::Pi,
            &json!({"hook_event_name":"Stop","stop_reason":"error"})
        ));
    }

    #[test]
    fn codex_system_and_ephemeral_stops_suppress_confirmation() {
        for input in [
            json!({"thread_source":"system", "transcript_path":"/tmp/rollout.jsonl"}),
            json!({"threadSource":"system", "transcriptPath":"/tmp/rollout.jsonl"}),
            json!({"thread_source":"ambient_suggestions", "transcript_path":"/tmp/rollout.jsonl"}),
            json!({"threadSource":"ambient_suggestions", "transcriptPath":"/tmp/rollout.jsonl"}),
        ] {
            assert_eq!(
                confirmation_suppression_reason(AgentKind::Codex, &input),
                Some("codex_blocked_thread_source"),
                "{input:?}"
            );
        }
        assert_eq!(
            confirmation_suppression_reason(AgentKind::Codex, &json!({"transcript_path":null})),
            Some("codex_transcript_path_null")
        );
        assert_eq!(
            confirmation_suppression_reason(AgentKind::Codex, &json!({"transcriptPath":null})),
            Some("codex_transcript_path_null")
        );
    }

    #[test]
    fn ordinary_and_non_codex_stops_keep_confirmation() {
        for input in [
            json!({"thread_source":"user", "transcript_path":"/tmp/rollout.jsonl"}),
            json!({"transcript_path":"/tmp/rollout.jsonl"}),
            json!({}),
        ] {
            assert_eq!(
                confirmation_suppression_reason(AgentKind::Codex, &input),
                None
            );
        }
        for kind in [
            AgentKind::Claude,
            AgentKind::Cursor,
            AgentKind::Grok,
            AgentKind::Pi,
        ] {
            assert_eq!(
                confirmation_suppression_reason(
                    kind,
                    &json!({"thread_source":"system", "transcript_path":null})
                ),
                None
            );
        }
    }

    #[test]
    fn maps_ask_results_fail_open() {
        assert_eq!(parse_ask_decision("not json", &[]), StopDecision::End);
        assert_eq!(parse_ask_decision("{}", &[]), StopDecision::End);
        assert_eq!(
            parse_ask_decision(r#"{"action":"cancel"}"#, &[]),
            StopDecision::End
        );
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[1],"user_input":"ignored"}]}"#,
                &[]
            ),
            StopDecision::End
        );
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[0]}]}"#,
                &[]
            ),
            StopDecision::Continue(None)
        );
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"user_input":"  next step  "}]}"#,
                &[]
            ),
            StopDecision::Continue(Some("next step".into()))
        );
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[],"user_input":""}]}"#,
                &[]
            ),
            StopDecision::End
        );
    }

    fn two_todos() -> Vec<crate::todos::TodoEntry> {
        vec![
            crate::todos::TodoEntry {
                id: "id-1".into(),
                text: "修复登录 bug".into(),
                created_at_ms: 1,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            },
            crate::todos::TodoEntry {
                id: "id-2".into(),
                text: "写发布说明".into(),
                created_at_ms: 2,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            },
        ]
    }

    #[test]
    fn stop_card_todo_mapping_follows_spec_d5() {
        let todos = two_todos();
        // 选待办 → 以该条为 continuation。
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[0]}]}"#,
                &todos
            ),
            StopDecision::Continue(Some("修复登录 bug".into()))
        );
        // 选待办 + 补充文字 → 按空行拼接。
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[1],"user_input":" 顺带更新截图 "}]}"#,
                &todos
            ),
            StopDecision::Continue(Some("写发布说明\n\n顺带更新截图".into()))
        );
        // 「继续对话」（索引后移到 todos.len()）→ 原有语义。
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[2]}]}"#,
                &todos
            ),
            StopDecision::Continue(None)
        );
        // 「结束对话」（末位）→ 结束并丢弃文字。
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"selected_indices":[3],"user_input":"ignored"}]}"#,
                &todos
            ),
            StopDecision::End
        );
        // 只填文字 → 继续（文字为指令），不涉及待办。
        assert_eq!(
            parse_ask_decision(
                r#"{"action":"answer","answers":[{"user_input":"先跑测试"}]}"#,
                &todos
            ),
            StopDecision::Continue(Some("先跑测试".into()))
        );
    }

    #[test]
    fn output_shapes_match_agents() {
        let claude = continuation_output(AgentKind::Claude, "continue");
        assert_eq!(claude["decision"], "block");
        assert_eq!(claude["reason"], "continue");
        let codex = continuation_output(AgentKind::Codex, "continue");
        assert_eq!(codex["decision"], "block");
        let cursor = continuation_output(AgentKind::Cursor, "continue");
        assert_eq!(cursor["followup_message"], "continue");
        let pi = continuation_output(AgentKind::Pi, "continue");
        assert_eq!(pi["followup_message"], "continue");
    }

    #[test]
    fn last_message_threshold_preserves_layout_and_unicode() {
        let short = "第一行\n第二行";
        assert_eq!(truncate_preserving_layout(short, 20), short);
        assert_eq!(
            last_assistant_message(
                AgentKind::Codex,
                &json!({"last_assistant_message":"\n  indented\n"})
            )
            .display
            .as_deref(),
            Some("\n  indented\n")
        );
        let long = "你".repeat(2_001);
        let out = truncate_preserving_layout(&long, 2_000);
        assert_eq!(out.matches('你').count(), 2_000);
        assert!(out.ends_with("… [truncated]"));
        let exact = "你".repeat(2_000);
        assert_eq!(truncate_preserving_layout(&exact, 2_000), exact);
        assert_eq!(
            last_assistant_message(AgentKind::Claude, &json!({"last_assistant_message":"  "})),
            LastAssistantMessage {
                display: None,
                user_confirmed_end_turn: false
            }
        );
    }

    #[test]
    fn confirmed_end_turn_marker_matches_any_substring_occurrence_and_is_stripped() {
        let marker = crate::prompts::USER_CONFIRMED_END_TURN_MARKER;
        for kind in [AgentKind::Claude, AgentKind::Codex, AgentKind::Pi] {
            let confirmed = last_assistant_message(
                kind,
                &json!({"last_assistant_message": format!("final report\n{marker}\n")}),
            );
            assert!(confirmed.user_confirmed_end_turn);
            assert_eq!(confirmed.display.as_deref(), Some("final report"));
        }

        // Anywhere in the body counts — including markdown glue and text after the marker.
        for text in [
            format!("done\n  {marker}  \n"),
            format!("prefix {marker}"),
            format!("quoted `{marker}`"),
            format!("**{marker}**"),
            format!("{marker}-suffix"),
            format!("结束。\n\n{marker}\n\n后面还有字"),
        ] {
            let message =
                last_assistant_message(AgentKind::Codex, &json!({"last_assistant_message": text}));
            assert!(message.user_confirmed_end_turn, "should match: {text:?}");
            assert!(
                !message.display.as_deref().unwrap_or("").contains(marker),
                "marker should be stripped from display: {text:?}"
            );
        }

        // Near-misses without the exact substring must not count.
        for text in [
            "user_confirmed_end_turn".to_string(),
            "[user_confirmed_end]".to_string(),
            "no marker here".to_string(),
        ] {
            let message =
                last_assistant_message(AgentKind::Codex, &json!({"last_assistant_message": text}));
            assert!(
                !message.user_confirmed_end_turn,
                "should not match: {text:?}"
            );
        }

        let long = format!("{}\n{marker}", "你".repeat(2_001));
        let message =
            last_assistant_message(AgentKind::Claude, &json!({"last_assistant_message": long}));
        assert!(message.user_confirmed_end_turn);
        assert_eq!(message.display.unwrap().matches('你').count(), 2_000);
    }

    #[test]
    fn internal_task_is_single_free_text_and_skips_history() {
        let task = build_task(
            AgentKind::Codex,
            "s1",
            "/tmp/p",
            &[],
            0,
            Some("done"),
            Lang::En,
        );
        assert!(task.single);
        assert!(!task.select_only);
        assert!(!task.record_history);
        assert_eq!(task.output_format, OutputFormat::Json);
        assert_eq!(task.questions[0].predefined_options.len(), 2);
        assert_eq!(task.message.text, "done");
        assert_eq!(task.project, "/tmp/p");
        assert!(task.questions[0].predefined_options[0].recommended);
        assert!(!task.questions[0].predefined_options[1].recommended);
    }

    #[test]
    fn stop_card_prepends_todo_chips_with_ids() {
        // 待办 chip 前置（带 id，供 Coordinator 出队）+ 原「继续/结束」压后（spec D5）。
        let task = build_task(
            AgentKind::Codex,
            "s1",
            "/tmp/p",
            &two_todos(),
            2,
            Some("done"),
            Lang::En,
        );
        let options = &task.questions[0].predefined_options;
        assert_eq!(options.len(), 4);
        // 未超上限 → 问题正文无溢出提示。
        assert!(!task.questions[0].message.contains("not listed"));
        // 展示前缀「Run todo: 」（与 whats-next 一致）；continuation 仍按索引取待办原文。
        assert_eq!(options[0].text, "Run todo: 修复登录 bug");
        assert_eq!(options[0].todo_id.as_deref(), Some("id-1"));
        assert!(!options[0].recommended);
        assert_eq!(options[1].todo_id.as_deref(), Some("id-2"));
        assert_eq!(options[2].text, "Continue conversation");
        assert!(options[2].recommended);
        assert!(options[2].todo_id.is_none());
        assert_eq!(options[3].text, "End conversation");
    }

    #[test]
    fn stop_card_appends_overflow_note_above_cap() {
        // 队列超上限（第 14 轮定案）：run_inner 截断后传入截断列表 + 原始总数；
        // 问题正文带溢出提示，选项数 = 上限 + 2。
        let todos: Vec<crate::todos::TodoEntry> = (0..crate::todos::MAX_OPTION_TODOS)
            .map(|i| crate::todos::TodoEntry {
                id: format!("id-{i}"),
                text: format!("task {i}"),
                created_at_ms: i as u64,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            })
            .collect();
        let task = build_task(
            AgentKind::Codex,
            "s1",
            "/tmp/p",
            &todos,
            crate::todos::MAX_OPTION_TODOS + 4,
            Some("done"),
            Lang::En,
        );
        let options = &task.questions[0].predefined_options;
        assert_eq!(options.len(), crate::todos::MAX_OPTION_TODOS + 2);
        assert!(
            task.questions[0].message.contains("4 more"),
            "{}",
            task.questions[0].message
        );
    }

    #[test]
    fn transcript_path_must_resolve_to_a_file_under_expected_root() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let inside_file = root.path().join("conversation.jsonl");
        let outside_file = outside.path().join("conversation.jsonl");
        std::fs::write(&inside_file, "{}").unwrap();
        std::fs::write(&outside_file, "{}").unwrap();
        assert_eq!(
            canonical_under(&inside_file, root.path()),
            std::fs::canonicalize(&inside_file).ok()
        );
        assert!(canonical_under(&outside_file, root.path()).is_none());
        assert!(canonical_under(&root.path().join("missing"), root.path()).is_none());
        assert!(canonical_under(root.path(), root.path()).is_none());
    }

    #[test]
    fn cursor_transcript_marker_is_detected_before_display_truncation() {
        let root = tempfile::tempdir().unwrap();
        let transcript = root.path().join("conversation.jsonl");
        let marker = crate::prompts::USER_CONFIRMED_END_TURN_MARKER;
        let event = json!({
            "role": "assistant",
            "message": {"content": [
                {"type": "text", "text": "你".repeat(2_001)},
                {"type": "text", "text": marker}
            ]}
        });
        std::fs::write(&transcript, format!("{event}\n")).unwrap();
        let raw = cursor_last_message_under(&json!({"transcript_path": transcript}), root.path());
        let message = normalize_last_assistant_message(raw.as_deref());
        assert!(message.user_confirmed_end_turn);
        assert_eq!(message.display.unwrap().matches('你').count(), 2_000);
    }
}
