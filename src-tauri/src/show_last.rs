//! Recover completed AskHuman exchanges (and optional last User Prompt) after Agent context
//! compaction. Output is an indented dialogue script (see `docs/specs/show-last-multi.md`).

use crate::agents::transcript_full::LastUserPrompt;
use crate::agents::AgentKind;
use crate::history::{HistoryAnswer, HistoryEntry, RecentSends};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub const MESSAGE_STDOUT_PREVIEW_BYTES: usize = 512;
pub const MAX_COUNT: usize = 10;

const MESSAGE_PREVIEW_OMISSION: &str = "… [middle omitted] …";

const PRIORITY_NOTE: &str = "\
priority note:
  A later AskHuman answer below is newer than the User Prompt. Use that
  AskHuman exchange as the current task — not the older User Prompt.";

#[derive(Debug, Clone)]
pub enum Scope {
    AgentSession {
        agent_kind: String,
        session_id: String,
    },
    McpInstance {
        mcp_instance_id: String,
        project: String,
    },
    Project(String),
}

impl Scope {
    fn storage_key(&self) -> String {
        match self {
            Scope::AgentSession {
                agent_kind,
                session_id,
            } => format!("session:{agent_kind}:{session_id}"),
            Scope::McpInstance {
                mcp_instance_id,
                project,
            } => format!("mcp:{mcp_instance_id}:{project}"),
            Scope::Project(project) => format!("project:{project}"),
        }
    }
}

/// Whether recovery was invoked from CLI or MCP (affects “for more” header wording).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Cli,
    Mcp,
}

/// Optional transcript session used to load the last User Prompt (independent of history scope).
#[derive(Debug, Clone)]
pub struct TranscriptHint {
    pub agent_kind: String,
    pub session_id: String,
}

#[derive(Debug)]
pub enum Error {
    HistoryDisabled,
    NotFound,
    InvalidCount,
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::HistoryDisabled => write!(f, "AskHuman history is disabled"),
            Error::NotFound => {
                write!(f, "No completed AskHuman exchange was found for this scope")
            }
            Error::InvalidCount => {
                write!(f, "count must be an integer between 1 and {MAX_COUNT}")
            }
            Error::Io(error) => write!(
                f,
                "Failed to prepare the recovered AskHuman exchange: {error}"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Parse and validate count (1..=MAX_COUNT).
pub fn parse_count(raw: Option<&str>) -> Result<usize, Error> {
    match raw {
        None => Ok(1),
        Some(s) => {
            let n: usize = s.parse().map_err(|_| Error::InvalidCount)?;
            if (1..=MAX_COUNT).contains(&n) {
                Ok(n)
            } else {
                Err(Error::InvalidCount)
            }
        }
    }
}

pub fn validate_count(n: usize) -> Result<usize, Error> {
    if (1..=MAX_COUNT).contains(&n) {
        Ok(n)
    } else {
        Err(Error::InvalidCount)
    }
}

/// Query and format recovered exchanges for `scope`.
pub fn recover(
    scope: &Scope,
    count: usize,
    surface: Surface,
    transcript: Option<&TranscriptHint>,
) -> Result<String, Error> {
    let history_limit = crate::config::AppConfig::load_without_secrets()
        .general
        .history_limit;
    recover_with(
        RecoverInput {
            scope,
            count,
            surface,
            transcript,
            history_limit,
            storage_dir: &crate::paths::show_last_dir(),
            now_ms: crate::history::now_ms(),
        },
        |scope, n| match scope {
            Scope::AgentSession {
                agent_kind,
                session_id,
            } => crate::history::recent_sends_for_session(agent_kind, session_id, n),
            Scope::McpInstance {
                mcp_instance_id,
                project,
            } => crate::history::recent_sends_for_mcp_instance(mcp_instance_id, project, n),
            Scope::Project(project) => crate::history::recent_sends_for_project(project, n),
        },
        |hint| {
            let kind = AgentKind::parse(&hint.agent_kind)?;
            crate::agents::transcript_full::last_timestamped_user_prompt(kind, &hint.session_id)
        },
    )
}

struct RecoverInput<'a> {
    scope: &'a Scope,
    count: usize,
    surface: Surface,
    transcript: Option<&'a TranscriptHint>,
    history_limit: u32,
    storage_dir: &'a Path,
    now_ms: i64,
}

fn recover_with(
    input: RecoverInput<'_>,
    lookup: impl FnOnce(&Scope, usize) -> RecentSends,
    load_prompt: impl FnOnce(&TranscriptHint) -> Option<LastUserPrompt>,
) -> Result<String, Error> {
    if input.history_limit == 0 {
        return Err(Error::HistoryDisabled);
    }
    let count = validate_count(input.count)?;
    let recent = lookup(input.scope, count);
    if recent.entries.is_empty() {
        return Err(Error::NotFound);
    }
    let prompt = input.transcript.and_then(load_prompt);
    let after_prompt_total = prompt
        .as_ref()
        .map(|p| count_after_prompt(input.scope, p.at_ms));
    render_timeline(TimelineInput {
        entries_newest_first: &recent.entries,
        total: recent.total,
        prompt: prompt.as_ref(),
        after_prompt_total,
        surface: input.surface,
        storage_key: &input.scope.storage_key(),
        storage_dir: input.storage_dir,
        now_ms: input.now_ms,
    })
    .map_err(Error::Io)
}

struct TimelineInput<'a> {
    entries_newest_first: &'a [HistoryEntry],
    total: usize,
    prompt: Option<&'a LastUserPrompt>,
    after_prompt_total: Option<usize>,
    surface: Surface,
    storage_key: &'a str,
    storage_dir: &'a Path,
    now_ms: i64,
}

fn count_after_prompt(scope: &Scope, after_ms: i64) -> usize {
    match scope {
        Scope::AgentSession {
            agent_kind,
            session_id,
        } => crate::history::count_sends_for_session_after(agent_kind, session_id, after_ms),
        Scope::McpInstance {
            mcp_instance_id,
            project,
        } => crate::history::count_sends_for_mcp_instance_after(mcp_instance_id, project, after_ms),
        Scope::Project(project) => crate::history::count_sends_for_project_after(project, after_ms),
    }
}

#[derive(Debug)]
enum TimelineItem<'a> {
    Exchange {
        entry: &'a HistoryEntry,
        /// Stable 1-based index in the full session (oldest Send = 1, newest = total).
        absolute_n: usize,
    },
    UserPrompt(&'a LastUserPrompt),
}

fn item_time_ms(item: &TimelineItem<'_>) -> i64 {
    match item {
        TimelineItem::Exchange { entry, .. } => entry.timestamp_ms,
        TimelineItem::UserPrompt(p) => p.at_ms,
    }
}

fn render_timeline(input: TimelineInput<'_>) -> std::io::Result<String> {
    let TimelineInput {
        entries_newest_first,
        total,
        prompt,
        after_prompt_total,
        surface,
        storage_key,
        storage_dir,
        now_ms,
    } = input;
    let shown = entries_newest_first.len();
    // Absolute numbers: newest-first rank 0 → total, rank 1 → total-1, …
    let mut items: Vec<TimelineItem<'_>> = entries_newest_first
        .iter()
        .enumerate()
        .map(|(rank, entry)| TimelineItem::Exchange {
            entry,
            absolute_n: total.saturating_sub(rank),
        })
        .collect();
    if let Some(p) = prompt {
        items.push(TimelineItem::UserPrompt(p));
    }
    // Chronological output (old → new); equal times: User Prompt before Exchange.
    items.sort_by(|a, b| {
        item_time_ms(a)
            .cmp(&item_time_ms(b))
            .then_with(|| match (a, b) {
                (TimelineItem::UserPrompt(_), TimelineItem::Exchange { .. }) => {
                    std::cmp::Ordering::Less
                }
                (TimelineItem::Exchange { .. }, TimelineItem::UserPrompt(_)) => {
                    std::cmp::Ordering::Greater
                }
                _ => std::cmp::Ordering::Equal,
            })
    });

    let has_prompt = prompt.is_some();
    let use_shell = shown >= 2 || has_prompt || total > shown;
    let has_later_exchange = prompt.is_some_and(|p| {
        entries_newest_first
            .iter()
            .any(|e| e.timestamp_ms > p.at_ms)
    });
    let shown_after_prompt = prompt
        .map(|p| {
            entries_newest_first
                .iter()
                .filter(|e| e.timestamp_ms > p.at_ms)
                .count()
        })
        .unwrap_or(0);
    let omitted_after_prompt = after_prompt_total
        .map(|t| t.saturating_sub(shown_after_prompt))
        .unwrap_or(0);

    let mut sections: Vec<String> = Vec::new();
    if use_shell {
        sections.push(format_header(shown));
    }

    for item in &items {
        match item {
            TimelineItem::Exchange { entry, absolute_n } => {
                let body = render_exchange_body(entry, storage_key, storage_dir, now_ms)?;
                if use_shell {
                    sections.push(format!(
                        "━━━━━━━━ Exchange #{absolute_n} ━━━━━━━━\n{}",
                        body.trim_end()
                    ));
                } else {
                    sections.push(body.trim_end().to_string());
                }
            }
            TimelineItem::UserPrompt(p) => {
                let mut block = format!(
                    "━━━━━━━━ User Prompt ━━━━━━━━\nsaid at: {}\n\n{}",
                    format_answered_at(p.at_ms, now_ms),
                    render_says_block(
                        "user says",
                        &p.text,
                        &[],
                        storage_key,
                        "user-prompt",
                        storage_dir,
                    )?
                );
                if has_later_exchange {
                    block.push_str("\n\n");
                    block.push_str(PRIORITY_NOTE);
                }
                if omitted_after_prompt > 0 {
                    block.push_str("\n\n");
                    block.push_str(&format_omitted_after_prompt(omitted_after_prompt, surface));
                }
                sections.push(block.trim_end().to_string());
            }
        }
    }

    let mut out = sections.join("\n\n");
    out.push('\n');
    Ok(out)
}

fn format_header(shown: usize) -> String {
    if shown == 1 {
        "show_last: 1 exchange".into()
    } else {
        format!("show_last: {shown} exchanges")
    }
}

fn format_omitted_after_prompt(omitted: usize, surface: Surface) -> String {
    let more = match surface {
        Surface::Cli => "use --show-last [N] for more",
        Surface::Mcp => "use count=[N] for more",
    };
    format!("… {omitted} AskHuman exchanges omitted after this prompt; {more} …")
}

fn render_exchange_body(
    entry: &HistoryEntry,
    storage_key: &str,
    storage_dir: &Path,
    now_ms: i64,
) -> std::io::Result<String> {
    let mut parts = Vec::new();
    parts.push(format!(
        "answered at: {}",
        format_answered_at(entry.timestamp_ms, now_ms)
    ));

    let message_files: Vec<&str> = entry
        .message
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    if !entry.message.text.is_empty() || !message_files.is_empty() {
        parts.push(render_says_block(
            "assistant (you) says",
            &entry.message.text,
            &message_files,
            storage_key,
            &entry.id,
            storage_dir,
        )?);
    }

    for (qi, question) in entry.questions.iter().enumerate() {
        let mut qa = String::new();
        qa.push_str(&format!("qa #{}\n", qi + 1));
        qa.push_str("  assistant (you) asked:\n");
        for line in question.message.lines() {
            qa.push_str("    ");
            qa.push_str(line);
            qa.push('\n');
        }
        if question.message.ends_with('\n') || question.message.is_empty() {
            // keep structure
        }
        // trim trailing single newline from asked body handling
        if !qa.ends_with('\n') {
            qa.push('\n');
        }
        qa.push_str(&render_user_answer(entry.answers.get(qi)));
        parts.push(qa.trim_end().to_string());
    }

    Ok(parts.join("\n\n") + "\n")
}

fn render_user_answer(answer: Option<&HistoryAnswer>) -> String {
    let Some(answer) = answer else {
        return "  user did not answer\n".into();
    };
    let mut fields = Vec::new();
    if !answer.selected_options.is_empty() {
        fields.push(format!(
            "    selected_options: {}",
            answer.selected_options.join(", ")
        ));
    }
    if let Some(input) = answer
        .user_input
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        let mut block = String::from("    user_input:\n");
        for line in input.lines() {
            block.push_str("      ");
            block.push_str(line);
            block.push('\n');
        }
        fields.push(block.trim_end().to_string());
    }
    let files: Vec<&str> = answer
        .images
        .iter()
        .chain(answer.files.iter())
        .map(String::as_str)
        .collect();
    if !files.is_empty() {
        let mut block = String::from("    files:\n");
        for f in files {
            block.push_str("      - ");
            block.push_str(f);
            block.push('\n');
        }
        fields.push(block.trim_end().to_string());
    }
    if fields.is_empty() {
        return "  user did not answer\n".into();
    }
    let mut out = String::from("  user answered:\n");
    out.push_str(&fields.join("\n"));
    out.push('\n');
    out
}

fn render_says_block(
    label: &str,
    text: &str,
    files: &[&str],
    storage_key: &str,
    file_id: &str,
    storage_dir: &Path,
) -> std::io::Result<String> {
    let mut out = format!("{label}:\n");
    if text.len() > MESSAGE_STDOUT_PREVIEW_BYTES {
        let path = write_full_message_at(storage_dir, storage_key, file_id, text)?;
        let preview = message_preview(text, MESSAGE_STDOUT_PREVIEW_BYTES);
        for line in preview.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        out.push_str(&format!("  full message: {}\n", path.display()));
    } else if !text.is_empty() {
        for line in text.lines() {
            out.push_str("  ");
            out.push_str(line);
            out.push('\n');
        }
        if text.ends_with('\n') {
            // empty trailing line already represented
        }
    }
    if !files.is_empty() {
        out.push_str("  files:\n");
        for f in files {
            out.push_str("    - ");
            out.push_str(f);
            out.push('\n');
        }
    }
    Ok(out.trim_end().to_string())
}

fn write_full_message_at(
    storage_dir: &Path,
    storage_key: &str,
    entry_id: &str,
    message: &str,
) -> std::io::Result<PathBuf> {
    let mut hasher = Sha256::new();
    hasher.update(storage_key.as_bytes());
    hasher.update([0]);
    hasher.update(entry_id.as_bytes());
    let digest = hasher.finalize();
    let filename = format!("{:x}.md", digest);
    let path = storage_dir.join(filename);
    crate::integrations::hook_edit::atomic_write_private(&path, message.as_bytes())
        .map_err(std::io::Error::other)?;
    Ok(path)
}

fn utf8_prefix(text: &str, max_bytes: usize) -> &str {
    let mut end = text.len().min(max_bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn utf8_suffix(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Keep both ends of a long recovery message. The latest User Prompt and AskHuman context often
/// state their subject near the beginning and the actionable request or conclusion near the end.
/// Prefer nearby paragraph, line, or sentence boundaries so Markdown is less likely to be cut
/// mid-item.
fn message_preview(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }

    let raw_head = utf8_prefix(text, max_bytes / 2);
    let head = prefer_head_boundary(raw_head).trim_end();
    // Give bytes released by a shorter, paragraph-aligned head to the tail. This avoids ugly
    // fragments when a message is only slightly over the limit while staying within 512 bytes.
    let raw_tail = utf8_suffix(text, max_bytes.saturating_sub(head.len()));
    let tail = prefer_tail_boundary(raw_tail).trim();

    format!("{head}\n\n{MESSAGE_PREVIEW_OMISSION}\n\n{tail}")
}

fn prefer_head_boundary(text: &str) -> &str {
    for separator in ["\n\n", "\n"] {
        if let Some(boundary) = text.rfind(separator).filter(|boundary| *boundary > 0) {
            return &text[..boundary];
        }
    }
    if let Some((boundary, punctuation)) = text.char_indices().rfind(|(boundary, punctuation)| {
        *boundary >= text.len() / 2 && is_sentence_boundary(*punctuation)
    }) {
        return &text[..boundary + punctuation.len_utf8()];
    }
    text
}

fn prefer_tail_boundary(text: &str) -> &str {
    for separator in ["\n\n", "\n"] {
        if let Some(boundary) = text.find(separator) {
            let start = boundary + separator.len();
            if start < text.len() {
                return &text[start..];
            }
        }
    }
    if let Some((boundary, punctuation)) = text.char_indices().find(|(boundary, punctuation)| {
        *boundary <= text.len() / 2 && is_sentence_boundary(*punctuation)
    }) {
        let start = boundary + punctuation.len_utf8();
        if start < text.len() {
            return &text[start..];
        }
    }
    text
}

fn is_sentence_boundary(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?' | ';' | '。' | '！' | '？' | '；')
}

fn format_answered_at(at_ms: i64, now_ms: i64) -> String {
    let relative = format_relative_en(at_ms, now_ms);
    let absolute = format_absolute_local(at_ms);
    format!("{relative} ({absolute})")
}

fn format_relative_en(at_ms: i64, now_ms: i64) -> String {
    let delta = (now_ms - at_ms).max(0) as u64;
    let secs = delta / 1000;
    if secs < 5 {
        return "just now".into();
    }
    if secs < 60 {
        return format!("{secs} seconds ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return if mins == 1 {
            "1 minute ago".into()
        } else {
            format!("{mins} minutes ago")
        };
    }
    let hours = mins / 60;
    if hours < 48 {
        return if hours == 1 {
            "1 hour ago".into()
        } else {
            format!("{hours} hours ago")
        };
    }
    let days = hours / 24;
    if days == 1 {
        "1 day ago".into()
    } else {
        format!("{days} days ago")
    }
}

fn format_absolute_local(at_ms: i64) -> String {
    let secs = at_ms.div_euclid(1000);
    crate::local_time::absolute(secs).unwrap_or_else(|| format_absolute_utc(secs))
}

fn format_absolute_utc(secs: i64) -> String {
    // Minimal UTC formatter without chrono.
    let days = secs.div_euclid(86400);
    let tod = secs.rem_euclid(86400) as u32;
    let h = tod / 3600;
    let m = (tod % 3600) / 60;
    let s = tod % 60;
    let (y, mo, d) = civil_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} +0000")
}

/// Howard Hinnant days_from_civil inverse: unix day count → y-m-d.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y as i32, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ChannelAction, FileAttachment, MessagePrompt, OptionItem, Question};

    fn sample(message: &str, ts: i64) -> HistoryEntry {
        HistoryEntry {
            id: format!("id-{ts}"),
            timestamp_ms: ts,
            project: "/p".into(),
            source: "Codex".into(),
            agent_kind: Some("codex".into()),
            agent_session_id: Some("s".into()),
            mcp_instance_id: Some("m".into()),
            channel: "popup".into(),
            action: ChannelAction::Send,
            is_markdown: true,
            message: MessagePrompt::new(message.into(), Vec::new()),
            questions: vec![Question::new(
                "Full question".into(),
                vec![OptionItem::new("Yes", true), OptionItem::new("No", false)],
            )],
            answers: vec![HistoryAnswer {
                selected_options: vec!["Yes".into()],
                user_input: Some("details".into()),
                images: vec!["/tmp/image.png".into()],
                files: vec!["/tmp/file.txt".into()],
            }],
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn render(
        entries: &[HistoryEntry],
        total: usize,
        prompt: Option<&LastUserPrompt>,
        after_prompt_total: Option<usize>,
        surface: Surface,
        storage_key: &str,
        storage_dir: &std::path::Path,
        now_ms: i64,
    ) -> String {
        render_timeline(TimelineInput {
            entries_newest_first: entries,
            total,
            prompt,
            after_prompt_total,
            surface,
            storage_key,
            storage_dir,
            now_ms,
        })
        .unwrap()
    }

    #[test]
    fn parse_count_defaults_and_bounds() {
        assert_eq!(parse_count(None).unwrap(), 1);
        assert_eq!(parse_count(Some("3")).unwrap(), 3);
        assert!(matches!(parse_count(Some("0")), Err(Error::InvalidCount)));
        assert!(matches!(parse_count(Some("11")), Err(Error::InvalidCount)));
        assert!(matches!(parse_count(Some("x")), Err(Error::InvalidCount)));
    }

    #[test]
    fn single_exchange_no_prompt_is_minimal() {
        let dir = tempfile::tempdir().unwrap();
        let entry = sample("Context", 1_000_000);
        let out = render(
            &[entry],
            1,
            None,
            None,
            Surface::Cli,
            "scope",
            dir.path(),
            1_000_000 + 120_000,
        );
        assert!(!out.contains("show_last:"));
        assert!(!out.contains("Exchange #"));
        assert!(out.contains("answered at: 2 minutes ago"));
        assert!(out.contains("assistant (you) says:\n  Context"));
        assert!(out.contains("qa #1\n  assistant (you) asked:\n    Full question"));
        assert!(out.contains("user answered:"));
        assert!(out.contains("selected_options: Yes"));
        assert!(out.contains("user_input:\n      details"));
        assert!(!out.contains("priority note"));
        assert!(!out.contains("No"));
        assert!(!out.contains("recommended"));
    }

    #[test]
    fn count_one_with_older_prompt_uses_shell_priority_note_and_omitted() {
        let dir = tempfile::tempdir().unwrap();
        let entry = sample("Context", 2_000_000);
        let prompt = LastUserPrompt {
            text: "帮我改成多条".into(),
            at_ms: 1_000_000,
        };
        // 12 total in session; 11 after prompt; showing 1 → omit 10; absolute # of newest is 12.
        let out = render(
            &[entry],
            12,
            Some(&prompt),
            Some(11),
            Surface::Cli,
            "scope",
            dir.path(),
            2_000_000 + 60_000,
        );
        assert!(out.starts_with("show_last: 1 exchange\n"));
        assert!(!out.contains("oldest first"));
        assert!(!out.contains(" of 12"));
        assert!(out.contains("━━━━━━━━ User Prompt ━━━━━━━━"));
        assert!(out.contains("user says:\n  帮我改成多条"));
        assert!(out.contains("\n\npriority note:\n"));
        assert!(out.contains(
            "… 10 AskHuman exchanges omitted after this prompt; use --show-last [N] for more …"
        ));
        assert!(out.contains("━━━━━━━━ Exchange #12 ━━━━━━━━"));
        let p = out.find("User Prompt").unwrap();
        let note = out.find("priority note:").unwrap();
        let omit = out.find("omitted after this prompt").unwrap();
        let e = out.find("Exchange #12").unwrap();
        assert!(p < note && note < omit && omit < e);
    }

    #[test]
    fn mcp_omitted_line_uses_count_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let entry = sample("x", 2_000_000);
        let prompt = LastUserPrompt {
            text: "hi".into(),
            at_ms: 1_000_000,
        };
        let out = render(
            &[entry],
            5,
            Some(&prompt),
            Some(4),
            Surface::Mcp,
            "scope",
            dir.path(),
            2_000_000,
        );
        assert!(out.starts_with("show_last: 1 exchange\n"));
        assert!(out.contains(
            "… 3 AskHuman exchanges omitted after this prompt; use count=[N] for more …"
        ));
        assert!(out.contains("Exchange #5"));
    }

    #[test]
    fn multi_exchange_uses_stable_absolute_numbers() {
        let dir = tempfile::tempdir().unwrap();
        let older = sample("old", 1_000_000);
        let newer = sample("new", 3_000_000);
        // newest-first input; total=10 so absolute ids are #9 and #10
        let out = render(
            &[newer, older],
            10,
            None,
            None,
            Surface::Cli,
            "scope",
            dir.path(),
            3_000_000,
        );
        assert!(out.starts_with("show_last: 2 exchanges\n"));
        assert!(!out.contains("oldest first"));
        let e9 = out.find("Exchange #9").unwrap();
        let e10 = out.find("Exchange #10").unwrap();
        assert!(e9 < e10);
        assert!(out[e9..e10].contains("assistant (you) says:\n  old"));
        assert!(out[e10..].contains("assistant (you) says:\n  new"));
    }

    #[test]
    fn empty_answer_and_multi_question() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = sample("", 1_000_000);
        entry.message.files = vec![FileAttachment {
            path: "/tmp/context.pdf".into(),
            name: "context.pdf".into(),
            size: 42,
            is_image: false,
        }];
        entry.questions.push(Question::new(
            "Second?".into(),
            vec![OptionItem::new("A", false)],
        ));
        entry.answers.push(HistoryAnswer {
            selected_options: Vec::new(),
            user_input: Some("  ".into()),
            images: Vec::new(),
            files: Vec::new(),
        });
        let out = render(
            &[entry],
            1,
            None,
            None,
            Surface::Cli,
            "scope",
            dir.path(),
            1_000_000,
        );
        assert!(out.contains("assistant (you) says:\n  files:\n    - /tmp/context.pdf"));
        assert!(out.contains("qa #2\n  assistant (you) asked:\n    Second?"));
        assert!(out.contains("user did not answer"));
    }

    #[test]
    fn long_message_single_says_block_with_full_message_path() {
        let dir = tempfile::tempdir().unwrap();
        let message = format!(
            "{}\n{}\n{}",
            "你".repeat(50),
            "中".repeat(300),
            "尾".repeat(50)
        );
        let entry = sample(&message, 1);
        let out = render(
            &[entry],
            1,
            None,
            None,
            Surface::Cli,
            "scope",
            dir.path(),
            1,
        );
        assert!(out.contains("assistant (you) says:"));
        assert!(out.contains(MESSAGE_PREVIEW_OMISSION));
        assert!(out.contains(&"你".repeat(50)));
        assert!(out.contains(&"尾".repeat(50)));
        assert!(!out.contains(&"中".repeat(100)));
        assert!(out.contains("full message:"));
        assert!(!out.contains("[message_truncated]"));
        let path_line = out
            .lines()
            .find(|l| l.trim_start().starts_with("full message:"))
            .unwrap();
        let path = path_line.split_once(':').unwrap().1.trim();
        assert_eq!(std::fs::read_to_string(path).unwrap(), message);
    }

    #[test]
    fn recovery_errors_and_history_disabled() {
        let scope = Scope::Project("/p".into());
        let dir = tempfile::tempdir().unwrap();
        let looked = std::cell::Cell::new(false);
        let disabled = recover_with(
            RecoverInput {
                scope: &scope,
                count: 1,
                surface: Surface::Cli,
                transcript: None,
                history_limit: 0,
                storage_dir: dir.path(),
                now_ms: 1,
            },
            |_, _| {
                looked.set(true);
                RecentSends {
                    entries: vec![sample("x", 1)],
                    total: 1,
                }
            },
            |_| None,
        );
        assert!(matches!(disabled, Err(Error::HistoryDisabled)));
        assert!(!looked.get());
        assert!(matches!(
            recover_with(
                RecoverInput {
                    scope: &scope,
                    count: 1,
                    surface: Surface::Cli,
                    transcript: None,
                    history_limit: 200,
                    storage_dir: dir.path(),
                    now_ms: 1,
                },
                |_, _| RecentSends {
                    entries: vec![],
                    total: 0
                },
                |_| None,
            ),
            Err(Error::NotFound)
        ));
    }

    #[test]
    fn scope_storage_keys_are_partitioned_and_stable() {
        let session = Scope::AgentSession {
            agent_kind: "codex".into(),
            session_id: "same".into(),
        };
        let mcp = Scope::McpInstance {
            mcp_instance_id: "same".into(),
            project: "/p".into(),
        };
        let project = Scope::Project("/p".into());
        assert_eq!(session.storage_key(), "session:codex:same");
        assert_eq!(mcp.storage_key(), "mcp:same:/p");
        assert_eq!(project.storage_key(), "project:/p");
    }

    #[test]
    fn utf8_prefix_never_splits_a_character() {
        assert_eq!(utf8_prefix("a你b", 2), "a");
        assert_eq!(utf8_prefix("a你b", 4), "a你");
        assert_eq!(utf8_suffix("a你b", 2), "b");
        assert_eq!(utf8_suffix("a你b", 4), "你b");
    }

    #[test]
    fn message_preview_prefers_line_boundaries_within_budget() {
        let head = "你".repeat(50);
        let tail = "尾".repeat(50);
        let text = format!("{head}\n{}\n{tail}", "中".repeat(300));
        let preview = message_preview(&text, MESSAGE_STDOUT_PREVIEW_BYTES);
        assert_eq!(
            preview,
            format!("{head}\n\n{MESSAGE_PREVIEW_OMISSION}\n\n{tail}")
        );
        assert!(preview.len() - MESSAGE_PREVIEW_OMISSION.len() <= MESSAGE_STDOUT_PREVIEW_BYTES);
    }

    #[test]
    fn message_preview_avoids_fragments_just_over_the_limit() {
        let head = "opening summary";
        let middle = "middle details ".repeat(25);
        let tail = "final conclusion ".repeat(8);
        let text = format!("{head}\n\n{middle}\n\n{tail}");
        assert!(text.len() > MESSAGE_STDOUT_PREVIEW_BYTES);

        let preview = message_preview(&text, MESSAGE_STDOUT_PREVIEW_BYTES);
        assert!(preview.starts_with(head));
        assert!(preview.ends_with(tail.trim_end()));
        assert!(!preview.contains("middle details"));
        assert!(preview.len() - MESSAGE_PREVIEW_OMISSION.len() <= MESSAGE_STDOUT_PREVIEW_BYTES);
    }

    #[test]
    fn message_preview_falls_back_to_sentence_boundaries() {
        assert_eq!(
            prefer_head_boundary("first。second。cut"),
            "first。second。"
        );
        assert_eq!(
            prefer_tail_boundary("fragment；final conclusion"),
            "final conclusion"
        );
    }

    #[test]
    fn newer_prompt_skips_priority_note() {
        let dir = tempfile::tempdir().unwrap();
        let entry = sample("Context", 1_000_000);
        let prompt = LastUserPrompt {
            text: "new task".into(),
            at_ms: 2_000_000,
        };
        let out = render(
            &[entry],
            1,
            Some(&prompt),
            Some(0),
            Surface::Cli,
            "scope",
            dir.path(),
            2_000_000,
        );
        assert!(!out.contains("priority note"));
        assert!(!out.contains("omitted after this prompt"));
        let e = out.find("Exchange #1").unwrap();
        let p = out.find("User Prompt").unwrap();
        assert!(e < p);
    }

    #[test]
    fn single_with_more_history_no_prompt_uses_singular_header_and_absolute_n() {
        let dir = tempfile::tempdir().unwrap();
        let entry = sample("only latest", 9_000_000);
        let out = render(
            &[entry],
            74,
            None,
            None,
            Surface::Cli,
            "scope",
            dir.path(),
            9_000_000,
        );
        assert!(out.starts_with("show_last: 1 exchange\n"));
        assert!(out.contains("━━━━━━━━ Exchange #74 ━━━━━━━━"));
        assert!(!out.contains("of 74"));
        assert!(!out.contains("oldest first"));
    }
}
