//! Hidden `PreToolUse` adapter for Claude Code's built-in `AskUserQuestion`
//! (spec `docs/specs/claude-ask-user-question.md`).
//!
//! Claude's own multiple-choice questions are handed to AskHuman so they can be answered from the
//! popup or any IM, then returned through the officially supported `allow` + `updatedInput` path.
//! Every failure that leaves the human without a way to answer writes nothing, so Claude falls back
//! to its native picker; only an explicit cancel becomes a `deny` that asks Claude to ask again.

use std::io::Read;
use std::time::Duration;

use serde_json::{json, Map, Value};

use crate::i18n::Lang;
use crate::models::{MessagePrompt, OptionItem, OutputFormat, Question};

/// Same ceilings as the permission adapter: hooks are short-lived and must not be a memory sink.
const MAX_STDIN_BYTES: u64 = 1024 * 1024;
const MAX_QUESTIONS: usize = 8;
const MAX_OPTIONS: usize = 24;
/// The human may be away; the hook waits as long as an ordinary ask does.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// One question as Claude sent it, kept alongside the text we rendered.
struct Prepared {
    /// Original question text — the key Claude expects in `answers`.
    original: String,
    single: bool,
    /// Rendered option text → original label. Options carry a description that is worth showing,
    /// but only the bare label may travel back to Claude.
    labels: Vec<(String, String)>,
}

/// Entry point for `AskHuman __ask-question-hook <agent>`. Prints at most one JSON object.
pub fn run(agent: Option<&str>) {
    if let Some(output) = run_inner(agent) {
        println!("{output}");
    }
}

fn run_inner(agent: Option<&str>) -> Option<String> {
    // Claude Code is the only agent with this tool.
    if !matches!(agent, Some("claude")) {
        return None;
    }
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_STDIN_BYTES {
        return None;
    }
    let input: Value = serde_json::from_slice(&bytes).ok()?;
    let object = input.as_object()?;
    if object.get("hook_event_name").and_then(Value::as_str) != Some("PreToolUse")
        || object.get("tool_name").and_then(Value::as_str) != Some("AskUserQuestion")
    {
        return None;
    }
    if !crate::integrations::agent_ask_question::enabled(crate::agents::AgentKind::Claude) {
        return None;
    }
    let tool_input = object.get("tool_input")?;
    let raw_questions = tool_input.get("questions").and_then(Value::as_array)?;
    let lang = Lang::current();
    let (questions, prepared) = build_questions(raw_questions, lang)?;

    let cwd = object
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default();
    let session_id = object
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let task = crate::ipc::TaskRequest {
        message: MessagePrompt::default(),
        questions,
        is_markdown: true,
        source: crate::models::source_name_for_agent(Some(crate::agents::AgentKind::Claude)),
        lang: lang.code().to_string(),
        project: cwd,
        select_only: false,
        // `multiSelect` is per question while our cards are single/multi as a whole (spec D5).
        single: false,
        output_format: OutputFormat::Json,
        record_history: true,
        agent_kind: Some(crate::agents::AgentKind::Claude.as_str().to_string()),
        agent_session_id: session_id,
        mcp_instance_id: None,
        agent_pid: None,
        caller_pid: std::process::id(),
        from_mcp: false,
        perf_id: String::new(),
        // Honour the perf harness switch like every other ask path, so automated runs can
        // exercise the cancel branch without a human clicking.
        perf_autodismiss: crate::perf::autodismiss(),
        whats_next: false,
    };

    let stdout = crate::client::run_ask_capture(task, ANSWER_TIMEOUT)?;
    let reply: Value = serde_json::from_str(stdout.trim()).ok()?;
    Some(decide(&reply, &prepared, tool_input, lang))
}

/// Map Claude's questions onto an AskHuman multi-question card (spec D4/D5).
fn build_questions(raw: &[Value], lang: Lang) -> Option<(Vec<Question>, Vec<Prepared>)> {
    if raw.is_empty() || raw.len() > MAX_QUESTIONS {
        return None;
    }
    let mut questions = Vec::new();
    let mut prepared = Vec::new();
    for item in raw {
        let text = item
            .get("question")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let header = item
            .get("header")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        let single = !item
            .get("multiSelect")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut labels: Vec<(String, String)> = Vec::new();
        let options: Vec<OptionItem> = item
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        let label = option
                            .get("label")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|label| !label.is_empty())?;
                        let description = option
                            .get("description")
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .unwrap_or_default();
                        let rendered = if description.is_empty() {
                            label.to_string()
                        } else {
                            format!("{label} · {description}")
                        };
                        labels.push((rendered.clone(), label.to_string()));
                        Some(OptionItem::new(rendered, false))
                    })
                    .take(MAX_OPTIONS)
                    .collect()
            })
            .unwrap_or_default();
        labels.truncate(options.len());
        let mut message = if header.is_empty() {
            text.to_string()
        } else {
            format!("{header} · {text}")
        };
        // Only whole-card multi-select exists, so single-select questions say so in their text.
        if single && options.len() > 1 {
            message.push(' ');
            message.push_str(crate::i18n::tr(lang, "askQuestion.pickOne"));
        }
        questions.push(Question::new(message, options));
        prepared.push(Prepared {
            original: text.to_string(),
            single,
            labels,
        });
    }
    Some((questions, prepared))
}

/// Turn the JSON reply into the hook decision (spec D6–D10). Pure so the mapping is unit-testable.
fn decide(reply: &Value, prepared: &[Prepared], tool_input: &Value, lang: Lang) -> String {
    let cancelled = reply.get("action").and_then(Value::as_str) == Some("cancel");
    let answers = if cancelled {
        Map::new()
    } else {
        collect_answers(reply, prepared)
    };
    if answers.is_empty() {
        // Explicit cancel, or a submit with nothing in it: ask Claude to ask again (spec D9).
        return json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": crate::i18n::tr(lang, "status.cancel"),
            }
        })
        .to_string();
    }
    // `updatedInput` replaces the whole input object, so the original questions ride along.
    let mut updated = tool_input.clone();
    if let Some(object) = updated.as_object_mut() {
        object.insert("answers".into(), Value::Object(answers));
    }
    json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "allow",
            "updatedInput": updated,
        }
    })
    .to_string()
}

/// `{原题面: 答案文本}`；未作答的题不写 key（spec D8）。
fn collect_answers(reply: &Value, prepared: &[Prepared]) -> Map<String, Value> {
    let mut answers = Map::new();
    for answer in reply
        .get("answers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let index = answer
            .get("question_index")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let Some(question) = prepared.get(index) else {
            continue;
        };
        // Selected text comes back as rendered ("label · description"); Claude only knows labels.
        let mut parts: Vec<String> = answer
            .get("selected_options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|selected| {
                        question
                            .labels
                            .iter()
                            .find(|(rendered, _)| rendered == selected)
                            .map(|(_, label)| label.clone())
                            .unwrap_or_else(|| selected.to_string())
                    })
                    .collect()
            })
            .unwrap_or_default();
        // A single-select question can only carry one label back (spec D5).
        if question.single {
            parts.truncate(1);
        }
        if let Some(text) = answer
            .get("user_input")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            parts.push(text.to_string());
        }
        let mut value = parts.join(", ");
        // Attachments cannot ride in `answers`; hand Claude the paths so it can read them (D7).
        for file in answer
            .get("files")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !value.is_empty() {
                value.push('\n');
            }
            value.push_str(file);
        }
        if value.is_empty() {
            continue;
        }
        answers.insert(question.original.clone(), Value::String(value));
    }
    answers
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_input() -> Value {
        json!({
            "questions": [
                { "question": "Which framework?", "header": "Framework",
                  "options": [
                      { "label": "React", "description": "Widest ecosystem" },
                      { "label": "Vue" }
                  ], "multiSelect": false },
                { "question": "Which extras?", "header": "",
                  "options": [{ "label": "Router" }, { "label": "Store" }], "multiSelect": true }
            ]
        })
    }

    fn prepared() -> Vec<Prepared> {
        build_questions(tool_input()["questions"].as_array().unwrap(), Lang::En)
            .unwrap()
            .1
    }

    fn answer(index: u64, options: &[&str], input: Option<&str>, files: &[&str]) -> Value {
        let mut value = json!({ "question_index": index });
        let object = value.as_object_mut().unwrap();
        if !options.is_empty() {
            object.insert("selected_options".into(), json!(options));
        }
        if let Some(input) = input {
            object.insert("user_input".into(), json!(input));
        }
        if !files.is_empty() {
            object.insert("files".into(), json!(files));
        }
        value
    }

    fn decided(reply: Value) -> Value {
        serde_json::from_str(&decide(&reply, &prepared(), &tool_input(), Lang::En)).unwrap()
    }

    #[test]
    fn questions_carry_header_prefix_and_single_select_note() {
        let (questions, prepared) =
            build_questions(tool_input()["questions"].as_array().unwrap(), Lang::En).unwrap();
        assert!(questions[0]
            .message
            .starts_with("Framework · Which framework?"));
        assert!(questions[0].message.len() > "Framework · Which framework?".len());
        // No header → no separator; multi-select → no note.
        assert_eq!(questions[1].message, "Which extras?");
        assert_eq!(questions[0].predefined_options.len(), 2);
        // Descriptions are worth reading, so they ride along in the option text.
        assert_eq!(
            questions[0].predefined_options[0].text,
            "React · Widest ecosystem"
        );
        assert_eq!(questions[0].predefined_options[1].text, "Vue");
        assert!(prepared[0].single);
        assert!(!prepared[1].single);
    }

    #[test]
    fn only_the_bare_label_travels_back_to_claude() {
        let value = decided(json!({
            "answers": [answer(0, &["React · Widest ecosystem"], None, &[])]
        }));
        assert_eq!(
            value["hookSpecificOutput"]["updatedInput"]["answers"]["Which framework?"],
            "React"
        );
    }

    #[test]
    fn a_question_without_text_rejects_the_whole_takeover() {
        let raw = json!([{ "question": "  ", "options": [] }]);
        assert!(build_questions(raw.as_array().unwrap(), Lang::En).is_none());
        assert!(build_questions(&[], Lang::En).is_none());
    }

    #[test]
    fn answers_are_keyed_by_the_original_question_text() {
        let value = decided(json!({
            "answers": [
                answer(0, &["React"], None, &[]),
                answer(1, &["Router", "Store"], None, &[]),
            ]
        }));
        let decision = &value["hookSpecificOutput"];
        assert_eq!(decision["permissionDecision"], "allow");
        let updated = &decision["updatedInput"];
        // The original questions must ride along: updatedInput replaces the whole input.
        assert_eq!(updated["questions"], tool_input()["questions"]);
        assert_eq!(updated["answers"]["Which framework?"], "React");
        assert_eq!(updated["answers"]["Which extras?"], "Router, Store");
    }

    #[test]
    fn single_select_keeps_only_the_first_label() {
        let value = decided(json!({ "answers": [answer(0, &["React", "Vue"], None, &[])] }));
        assert_eq!(
            value["hookSpecificOutput"]["updatedInput"]["answers"]["Which framework?"],
            "React"
        );
    }

    #[test]
    fn free_text_and_options_are_joined() {
        let only_text = decided(json!({ "answers": [answer(0, &[], Some("Svelte"), &[])] }));
        assert_eq!(
            only_text["hookSpecificOutput"]["updatedInput"]["answers"]["Which framework?"],
            "Svelte"
        );

        let both = decided(json!({ "answers": [answer(0, &["React"], Some("with RSC"), &[])] }));
        assert_eq!(
            both["hookSpecificOutput"]["updatedInput"]["answers"]["Which framework?"],
            "React, with RSC"
        );
    }

    #[test]
    fn attachments_are_appended_as_paths() {
        let value = decided(json!({
            "answers": [answer(0, &["React"], None, &["/tmp/a.png", "/tmp/b.pdf"])]
        }));
        assert_eq!(
            value["hookSpecificOutput"]["updatedInput"]["answers"]["Which framework?"],
            "React\n/tmp/a.png\n/tmp/b.pdf"
        );
    }

    #[test]
    fn unanswered_questions_are_simply_absent() {
        let value = decided(json!({ "answers": [answer(1, &["Router"], None, &[])] }));
        let answers = &value["hookSpecificOutput"]["updatedInput"]["answers"];
        assert_eq!(answers["Which extras?"], "Router");
        assert!(answers.get("Which framework?").is_none());
    }

    #[test]
    fn cancel_and_empty_submit_both_ask_claude_to_ask_again() {
        for reply in [
            json!({ "action": "cancel", "status": "The user canceled." }),
            json!({ "answers": [] }),
            json!({ "answers": [answer(0, &[], Some("   "), &[])] }),
        ] {
            let decision = decided(reply)["hookSpecificOutput"].clone();
            assert_eq!(decision["permissionDecision"], "deny");
            assert!(decision["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .contains("ask again"));
            assert!(decision.get("updatedInput").is_none());
        }
    }
}
