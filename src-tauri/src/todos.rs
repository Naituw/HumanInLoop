//! Project-scoped todo queue (spec `docs/specs/todo-whats-next.md` D1).
//!
//! `~/.askhuman/state/todos.json` is the single source of truth: every process reads and
//! writes the file directly (no daemon-resident state — todos have no hot path). Mutations
//! take an exclusive advisory lock (`todos.lock`, same pattern as the history write lock;
//! best-effort no-op off Unix) around the read-modify-write, and the file itself is written
//! atomically (tmp + rename). Empty project keys are pruned on write.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One pending todo entry. FIFO order is the `Vec` order in the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoEntry {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub created_at_ms: u64,
    /// Agent family that invoked CLI `todo add`; absent for human/GUI/IM additions and old data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// 自动执行（第 17 轮定案）：whats-next 时不提问、直接把最靠前的自动待办作为下一个任务返回。
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto: bool,
    /// Files and images owned by or referenced from this todo.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::todo_attachments::TodoAttachment>,
}

/// 一条已执行的历史待办（第 16 轮定案：仅「执行出队」进历史，手动删除/清空不记）。
/// 文件内按完成时间正序追加，展示端倒序。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneTodoEntry {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub created_at_ms: u64,
    /// Preserved from the pending entry so execution history retains its origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    #[serde(default)]
    pub done_at_ms: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<crate::todo_attachments::TodoAttachment>,
}

/// 选项类展示点（whats-next 卡 / Stop 卡 / IM `/todo-rm` 删除卡）最多列出的待办条数
/// （第 14 轮定案：前 10 条 + 溢出提示；TG 键盘 ≤100、Slack actions ≤25 等渠道硬限制的安全值）。
/// 顺序靠 GUI 待办窗口拖拽调整，头部优先展示。
pub const MAX_OPTION_TODOS: usize = 10;

/// 溢出提示（第 14 轮定案）：附在问题/卡片正文尾部；`total` ≤ 上限时 None。
pub fn overflow_note(total: usize, lang: crate::i18n::Lang) -> Option<String> {
    (total > MAX_OPTION_TODOS).then(|| {
        crate::i18n::tr(lang, "todo.moreNote")
            .replace("{n}", &(total - MAX_OPTION_TODOS).to_string())
    })
}

pub fn attachment_badge(lang: crate::i18n::Lang, count: usize) -> String {
    crate::i18n::tr(lang, "todo.attachmentBadge").replace("{n}", &count.to_string())
}

/// Split a generated attachment badge from a todo option label for rich channel rendering.
/// Requiring the exact localized count shape avoids treating arbitrary bracket text as a badge.
pub fn split_attachment_badge(text: &str) -> (&str, Option<&str>) {
    let Some(badge_start) = text.rfind(" 【") else {
        return (text, None);
    };
    let body = &text[..badge_start];
    let badge = &text[badge_start + 1..];
    let Some(inner) = badge
        .strip_prefix('【')
        .and_then(|value| value.strip_suffix('】'))
    else {
        return (text, None);
    };
    let count = inner
        .strip_suffix(" 个附件")
        .or_else(|| inner.strip_suffix(" attachments"));
    if count.is_some_and(|value| value.parse::<usize>().is_ok()) {
        (body, Some(badge))
    } else {
        (text, None)
    }
}

pub fn option_label(lang: crate::i18n::Lang, entry: &TodoEntry) -> String {
    let badge = if entry.attachments.is_empty() {
        String::new()
    } else {
        format!(" {}", attachment_badge(lang, entry.attachments.len()))
    };
    format!(
        "{}{}{badge}",
        crate::i18n::tr(lang, "whatsNext.todoPrefix"),
        entry.text
    )
}

/// On-disk shape: project key (git root path) → FIFO entries, plus per-project execution
/// history (round 16; capped by the `todo_history_limit` setting at record time).
#[derive(Default, Serialize, Deserialize)]
struct TodoFile {
    #[serde(default)]
    projects: HashMap<String, Vec<TodoEntry>>,
    #[serde(default)]
    history: HashMap<String, Vec<DoneTodoEntry>>,
}

fn todos_file() -> PathBuf {
    crate::paths::state_dir().join("todos.json")
}

fn todos_lock() -> PathBuf {
    crate::paths::state_dir().join("todos.lock")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn load_at(path: &Path) -> TodoFile {
    let Ok(text) = std::fs::read_to_string(path) else {
        return TodoFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Atomic write; prunes projects with no entries.
/// Returns `true` only when the file was fully replaced (tmp write + rename both ok).
/// Callers that must not report a false success (CLI / MCP `todo_add`) should surface
/// `false` as a hard error; other mutators keep best-effort behavior.
fn store_at(path: &Path, mut data: TodoFile) -> bool {
    data.projects.retain(|_, entries| !entries.is_empty());
    data.history.retain(|_, entries| !entries.is_empty());
    let Ok(json) = serde_json::to_string_pretty(&data) else {
        return false;
    };
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    if std::fs::write(&tmp, json.as_bytes()).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, path).is_ok() {
        true
    } else {
        let _ = std::fs::remove_file(&tmp);
        false
    }
}

// ===== Cross-process write lock (same pattern as history.rs) =====

fn lock_at(path: &Path) -> Option<crate::file_lock::FileLock> {
    crate::file_lock::FileLock::exclusive(path).ok()
}

/// Normalize a project key/text pair for storage; `None` when unusable.
fn normalized(project: &str, text: &str) -> Option<(String, String)> {
    let project = project.trim();
    let text = text.trim();
    (!project.is_empty() && !text.is_empty()).then(|| (project.to_string(), text.to_string()))
}

// ===== Public API (default paths) =====

/// Pending todos of a project (FIFO). Missing file / unknown project → empty.
pub fn list(project: &str) -> Vec<TodoEntry> {
    list_at(&todos_file(), project)
}

/// Full snapshot: project key → entries (GUI window / project selector).
pub fn all() -> HashMap<String, Vec<TodoEntry>> {
    load_at(&todos_file()).projects
}

pub fn cleanup_attachment_orphans() {
    let data = load_at(&todos_file());
    let mut referenced = std::collections::HashSet::new();
    for attachment in data
        .projects
        .values()
        .flatten()
        .flat_map(|entry| entry.attachments.iter())
        .chain(
            data.history
                .values()
                .flatten()
                .flat_map(|entry| entry.attachments.iter()),
        )
    {
        referenced.insert(PathBuf::from(&attachment.path));
        if let Some(thumbnail) = &attachment.thumbnail_path {
            referenced.insert(PathBuf::from(thumbnail));
        }
    }
    crate::todo_attachments::cleanup_orphans(&referenced);
}

/// Why [`add`] / [`add_auto`] / [`add_from_agent`] failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddError {
    /// Project key or text empty after trim.
    EmptyInput,
    /// Could not persist `todos.json` (permissions, sandbox, disk full, …).
    Persist,
    /// One or more attachment paths could not be prepared.
    Attachment(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    NotFound,
    EmptyText,
    Conflict,
    UnknownAttachment,
    Persist,
    Attachment(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MutationError {
    Persist,
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Persist => write!(f, "failed to persist todos.json"),
        }
    }
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "todo not found"),
            Self::EmptyText => write!(f, "todo text must not be empty"),
            Self::Conflict => write!(f, "todo changed while it was being edited"),
            Self::UnknownAttachment => write!(f, "attachment does not belong to this todo"),
            Self::Persist => write!(f, "failed to persist todos.json"),
            Self::Attachment(message) => f.write_str(message),
        }
    }
}

/// Append one entry. Errors when input is empty or the write did not land on disk.
pub fn add(project: &str, text: &str) -> Result<TodoEntry, AddError> {
    add_at(&todos_file(), &todos_lock(), project, text)
}

/// Append one auto-run entry (round 17; CLI `todo add --auto` / GUI toggle / IM `/todo-auto`).
pub fn add_auto(project: &str, text: &str) -> Result<TodoEntry, AddError> {
    add_auto_at(&todos_file(), &todos_lock(), project, text)
}

/// Append an entry created by a recognized Agent CLI invocation.
pub fn add_from_agent(
    project: &str,
    text: &str,
    auto: bool,
    agent: crate::agents::AgentKind,
) -> Result<TodoEntry, AddError> {
    add_impl(
        &todos_file(),
        &todos_lock(),
        project,
        text,
        auto,
        Some(agent.as_str()),
    )
}

/// Append a GUI/CLI/MCP todo with files resolved relative to `cwd`.
pub fn add_with_attachments(
    project: &str,
    text: &str,
    auto: bool,
    agent_kind: Option<crate::agents::AgentKind>,
    raw_paths: &[String],
    cwd: &Path,
) -> Result<TodoEntry, AddError> {
    add_impl_with_attachments(
        &todos_file(),
        &todos_lock(),
        project,
        text,
        auto,
        agent_kind.map(|agent| agent.as_str()),
        raw_paths,
        cwd,
    )
}

/// Atomically save GUI text and attachment edits with optimistic concurrency checks.
#[allow(clippy::too_many_arguments)]
pub fn update_with_attachments(
    project: &str,
    id: &str,
    expected_text: &str,
    expected_attachment_ids: &[String],
    text: &str,
    keep_attachment_ids: &[String],
    add_paths: &[String],
    cwd: &Path,
) -> Result<TodoEntry, UpdateError> {
    update_with_attachments_at(
        &todos_file(),
        &todos_lock(),
        project,
        id,
        expected_text,
        expected_attachment_ids,
        text,
        keep_attachment_ids,
        add_paths,
        cwd,
    )
}

/// Add/remove attachments by stable ids (MCP). Existing text and auto state are preserved.
pub fn update_attachment_ids(
    project: &str,
    id: &str,
    add_paths: &[String],
    remove_attachment_ids: &[String],
    cwd: &Path,
) -> Result<TodoEntry, UpdateError> {
    let current = list(project)
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or(UpdateError::NotFound)?;
    let remove: std::collections::HashSet<&str> =
        remove_attachment_ids.iter().map(String::as_str).collect();
    if remove.iter().any(|id| {
        !current
            .attachments
            .iter()
            .any(|attachment| attachment.id == *id)
    }) {
        return Err(UpdateError::UnknownAttachment);
    }
    let expected_ids: Vec<String> = current
        .attachments
        .iter()
        .map(|attachment| attachment.id.clone())
        .collect();
    let keep_ids: Vec<String> = current
        .attachments
        .iter()
        .filter(|attachment| !remove.contains(attachment.id.as_str()))
        .map(|attachment| attachment.id.clone())
        .collect();
    update_with_attachments(
        project,
        id,
        &current.text,
        &expected_ids,
        &current.text,
        &keep_ids,
        add_paths,
        cwd,
    )
}

/// 1-based list index of `id` in the project's pending queue, if present.
pub fn index_of(project: &str, id: &str) -> Option<usize> {
    list(project).iter().position(|e| e.id == id).map(|i| i + 1)
}

/// Toggle the auto-run flag of one entry (round 17). Returns the new flag, `None` if missing.
pub fn set_auto(project: &str, id: &str, auto: bool) -> Option<bool> {
    set_auto_at(&todos_file(), &todos_lock(), project, id, auto)
}

/// Update the text of one pending entry (GUI double-click edit). Keeps id / created_at / auto /
/// agent_kind. Returns the stored text after trim, or `None` if the entry is missing or the
/// new text is empty after trim.
pub fn set_text(project: &str, id: &str, text: &str) -> Option<String> {
    set_text_at(&todos_file(), &todos_lock(), project, id, text)
}

/// Front-most auto-run entry of a project (whats-next auto-dispatch, round 17).
pub fn first_auto(project: &str) -> Option<TodoEntry> {
    list(project).into_iter().find(|e| e.auto)
}

/// Remove one entry by id. Returns whether it existed.
pub fn remove(project: &str, id: &str) -> bool {
    remove_at(&todos_file(), &todos_lock(), project, id)
}

/// User-facing remove variant that distinguishes a missing entry from a failed disk commit.
pub fn remove_checked(project: &str, id: &str) -> Result<bool, MutationError> {
    remove_checked_at(&todos_file(), &todos_lock(), project, id)
}

/// Clear a project's queue; returns how many entries were removed.
pub fn clear(project: &str) -> usize {
    clear_at(&todos_file(), &todos_lock(), project)
}

/// User-facing clear variant that never reports removed entries when the disk commit failed.
pub fn clear_checked(project: &str) -> Result<usize, MutationError> {
    clear_checked_at(&todos_file(), &todos_lock(), project)
}

/// Dequeue entries by id (best-effort: missing ids are skipped, spec D11). Returns the
/// entries actually removed. This is the "started executing → auto-clear" point, and the only
/// path that records execution history (round 16; capped by the `todo_history_limit` setting,
/// `0` = stop recording, existing history kept — same semantics as the reply history limit).
pub fn take(project: &str, ids: &[String]) -> Vec<TodoEntry> {
    let limit = crate::config::AppConfig::load_without_secrets()
        .general
        .todo_history_limit as usize;
    take_at(&todos_file(), &todos_lock(), project, ids, limit)
}

/// Execution history of a project, newest first (GUI history section).
pub fn history(project: &str) -> Vec<DoneTodoEntry> {
    history_at(&todos_file(), project)
}

/// Move a history entry back to the end of the pending queue (GUI "restore", round 16).
/// Returns whether the entry existed in history.
pub fn restore(project: &str, id: &str) -> bool {
    restore_at(&todos_file(), &todos_lock(), project, id)
}

/// Clear a project's execution history (GUI history section, round 18). Returns the number
/// of entries removed.
pub fn clear_history(project: &str) -> usize {
    clear_history_at(&todos_file(), &todos_lock(), project)
}

pub fn clear_history_checked(project: &str) -> Result<usize, MutationError> {
    clear_history_checked_at(&todos_file(), &todos_lock(), project)
}

/// Reorder a project's queue to match `ids` (GUI drag handle, round 14). Best-effort under
/// concurrent add/remove: unknown ids are ignored, entries missing from `ids` keep their
/// relative order after the listed ones. Returns whether the stored order changed.
pub fn reorder(project: &str, ids: &[String]) -> bool {
    reorder_at(&todos_file(), &todos_lock(), project, ids)
}

/// Collect the todo ids a terminal answer consumed (pure function, spec D2/D5/D7).
///
/// Two sources, deduplicated:
/// - options carrying a `todo_id` whose text was selected (whats-next / Stop-card chips;
///   channels only report the option text, so ids are recovered from the request);
/// - explicit `QuestionAnswer.todo_ids` (popup collapsible todo section).
///
/// The caller (Coordinator, at the first-terminal convergence point) passes the result to
/// [`take`]; missing ids are skipped there (best-effort, spec D11).
pub fn ids_to_dequeue(
    request: &crate::models::AskRequest,
    result: &crate::models::ChannelResult,
) -> Vec<String> {
    if result.action != crate::models::ChannelAction::Send {
        return Vec::new();
    }
    let mut ids: Vec<String> = Vec::new();
    for (i, answer) in result.answers.iter().enumerate() {
        let options = request
            .questions
            .get(i)
            .map(|q| q.predefined_options.as_slice())
            .unwrap_or(&[]);
        for sel in &answer.selected_options {
            if let Some(id) = options
                .iter()
                .find(|o| &o.text == sel)
                .and_then(|o| o.todo_id.clone())
            {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        for id in &answer.todo_ids {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        for selection in &answer.todo_selections {
            if !ids.contains(&selection.id) {
                ids.push(selection.id.clone());
            }
        }
    }
    ids
}

/// Resolve attachment snapshots selected by the winning answer into delivery paths before the
/// corresponding todos are dequeued. Managed files are copied into the request's 24-hour temp
/// area; references are passed through. Missing/removed attachments become explicit task warnings.
pub fn apply_todo_deliveries(
    request: &crate::models::AskRequest,
    result: &mut crate::models::ChannelResult,
    project: &str,
) {
    apply_todo_deliveries_with(request, result, |todo_id, expected| {
        prepare_delivery_consistent(project, todo_id, expected, &request.id)
    });
}

/// Prepare outside `todos.lock`, then briefly revalidate attachment membership under the lock.
/// A mutation that commits first forces a retry; a mutation that acquires the lock after a
/// successful revalidation cannot retract the already-created delivery copy.
pub fn prepare_delivery_consistent(
    project: &str,
    todo_id: &str,
    expected: &[crate::todo_attachments::TodoAttachmentSnapshot],
    request_id: &str,
) -> crate::todo_attachments::TodoDelivery {
    const MAX_ATTEMPTS: usize = 3;

    for _ in 0..MAX_ATTEMPTS {
        let snapshot = list(project)
            .into_iter()
            .find(|entry| entry.id == todo_id)
            .map(|entry| entry.attachments);
        let delivery = crate::todo_attachments::prepare_delivery(
            todo_id,
            expected,
            snapshot.as_deref(),
            request_id,
        );

        let _guard = lock_at(&todos_lock());
        let latest = load_at(&todos_file())
            .projects
            .get(project.trim())
            .and_then(|entries| entries.iter().find(|entry| entry.id == todo_id))
            .map(|entry| entry.attachments.clone());
        if latest.as_ref() == snapshot.as_ref() {
            return delivery;
        }
        drop(_guard);
        crate::todo_attachments::cleanup_delivery(request_id);
    }

    crate::todo_attachments::TodoDelivery {
        files: Vec::new(),
        warnings: expected
            .iter()
            .map(|attachment| {
                format!(
                    "Todo attachment changed while delivery was being prepared: {} ({})",
                    attachment.name, attachment.source_path
                )
            })
            .collect(),
    }
}

fn apply_todo_deliveries_with(
    request: &crate::models::AskRequest,
    result: &mut crate::models::ChannelResult,
    mut prepare: impl FnMut(
        &str,
        &[crate::todo_attachments::TodoAttachmentSnapshot],
    ) -> crate::todo_attachments::TodoDelivery,
) {
    if result.action != crate::models::ChannelAction::Send {
        return;
    }
    for (question_index, answer) in result.answers.iter_mut().enumerate() {
        let options = request
            .questions
            .get(question_index)
            .map(|question| question.predefined_options.as_slice())
            .unwrap_or(&[]);
        let mut selections: Vec<(String, Vec<crate::todo_attachments::TodoAttachmentSnapshot>)> =
            Vec::new();

        for selected in &mut answer.selected_options {
            let Some(option) = options.iter().find(|option| option.text == *selected) else {
                continue;
            };
            let Some(todo_id) = &option.todo_id else {
                continue;
            };
            selections.push((todo_id.clone(), option.todo_attachments.clone()));
        }
        for selection in &answer.todo_selections {
            if let Some(existing) = selections.iter_mut().find(|item| item.0 == selection.id) {
                if existing.1.is_empty() {
                    existing.1 = selection.attachments.clone();
                }
            } else {
                selections.push((selection.id.clone(), selection.attachments.clone()));
            }
        }

        let mut warnings = Vec::new();
        for (todo_id, expected) in selections {
            let delivery = prepare(&todo_id, &expected);
            for file in delivery.files {
                if !answer.files.contains(&file) {
                    answer.files.push(file);
                }
            }
            warnings.extend(delivery.warnings);
        }
        if let Some(block) = crate::todo_attachments::warning_block(&warnings) {
            let input = answer.user_input.get_or_insert_with(String::new);
            if !input.trim().is_empty() {
                input.push_str("\n\n");
            }
            input.push_str(&block);
        }
    }
}

#[cfg(test)]
fn apply_todo_deliveries_from_entries(
    request: &crate::models::AskRequest,
    result: &mut crate::models::ChannelResult,
    current: &[TodoEntry],
) {
    apply_todo_deliveries_with(request, result, |todo_id, expected| {
        let current_attachments = current
            .iter()
            .find(|entry| entry.id == todo_id)
            .map(|entry| entry.attachments.as_slice());
        crate::todo_attachments::prepare_delivery(
            todo_id,
            expected,
            current_attachments,
            &request.id,
        )
    });
}

// ===== Path-parameterized implementations (unit-testable without touching the real home) =====

pub fn list_at(path: &Path, project: &str) -> Vec<TodoEntry> {
    load_at(path)
        .projects
        .get(project.trim())
        .cloned()
        .unwrap_or_default()
}

pub fn add_at(path: &Path, lock: &Path, project: &str, text: &str) -> Result<TodoEntry, AddError> {
    add_impl(path, lock, project, text, false, None)
}

pub fn add_auto_at(
    path: &Path,
    lock: &Path,
    project: &str,
    text: &str,
) -> Result<TodoEntry, AddError> {
    add_impl(path, lock, project, text, true, None)
}

fn add_impl(
    path: &Path,
    lock: &Path,
    project: &str,
    text: &str,
    auto: bool,
    agent_kind: Option<&str>,
) -> Result<TodoEntry, AddError> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    add_impl_with_attachments(path, lock, project, text, auto, agent_kind, &[], &cwd)
}

#[allow(clippy::too_many_arguments)]
fn add_impl_with_attachments(
    path: &Path,
    lock: &Path,
    project: &str,
    text: &str,
    auto: bool,
    agent_kind: Option<&str>,
    raw_paths: &[String],
    cwd: &Path,
) -> Result<TodoEntry, AddError> {
    let (project, text) = normalized(project, text).ok_or(AddError::EmptyInput)?;
    let id = uuid::Uuid::new_v4().to_string();
    let mut staged = crate::todo_attachments::stage_attachments(&id, raw_paths, &[], cwd)
        .map_err(|error| AddError::Attachment(error.to_string()))?;
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let entry = TodoEntry {
        id,
        text,
        created_at_ms: now_ms(),
        agent_kind: agent_kind.map(str::to_string),
        auto,
        attachments: staged.attachments.clone(),
    };
    staged
        .commit_files()
        .map_err(|error| AddError::Attachment(error.to_string()))?;
    data.projects
        .entry(project.clone())
        .or_default()
        .push(entry.clone());
    if !store_at(path, data) {
        staged.rollback_committed();
        return Err(AddError::Persist);
    }
    // Defense in depth: never claim success unless the entry is readable back.
    if !list_at(path, &project).iter().any(|e| e.id == entry.id) {
        return Err(AddError::Persist);
    }
    Ok(entry)
}

#[allow(clippy::too_many_arguments)]
fn update_with_attachments_at(
    path: &Path,
    lock: &Path,
    project: &str,
    id: &str,
    expected_text: &str,
    expected_attachment_ids: &[String],
    text: &str,
    keep_attachment_ids: &[String],
    add_paths: &[String],
    cwd: &Path,
) -> Result<TodoEntry, UpdateError> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err(UpdateError::EmptyText);
    }

    let snapshot = list_at(path, project)
        .into_iter()
        .find(|entry| entry.id == id)
        .ok_or(UpdateError::NotFound)?;
    let snapshot_ids: Vec<String> = snapshot
        .attachments
        .iter()
        .map(|attachment| attachment.id.clone())
        .collect();
    if snapshot.text != expected_text || snapshot_ids != expected_attachment_ids {
        return Err(UpdateError::Conflict);
    }
    let keep: std::collections::HashSet<&str> =
        keep_attachment_ids.iter().map(String::as_str).collect();
    if keep.iter().any(|id| {
        !snapshot
            .attachments
            .iter()
            .any(|attachment| attachment.id == *id)
    }) {
        return Err(UpdateError::UnknownAttachment);
    }
    let kept: Vec<crate::todo_attachments::TodoAttachment> = snapshot
        .attachments
        .iter()
        .filter(|attachment| keep.contains(attachment.id.as_str()))
        .cloned()
        .collect();
    let removed: Vec<crate::todo_attachments::TodoAttachment> = snapshot
        .attachments
        .iter()
        .filter(|attachment| !keep.contains(attachment.id.as_str()))
        .cloned()
        .collect();
    let mut staged = crate::todo_attachments::stage_attachments(id, add_paths, &kept, cwd)
        .map_err(|error| UpdateError::Attachment(error.to_string()))?;

    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let entry = data
        .projects
        .get_mut(project.trim())
        .and_then(|entries| entries.iter_mut().find(|entry| entry.id == id))
        .ok_or(UpdateError::NotFound)?;
    let current_ids: Vec<String> = entry
        .attachments
        .iter()
        .map(|attachment| attachment.id.clone())
        .collect();
    if entry.text != expected_text || current_ids != expected_attachment_ids {
        return Err(UpdateError::Conflict);
    }

    staged
        .commit_files()
        .map_err(|error| UpdateError::Attachment(error.to_string()))?;
    entry.text = text;
    entry.attachments = kept;
    entry.attachments.extend(staged.attachments.clone());
    let updated = entry.clone();
    if !store_at(path, data) {
        staged.rollback_committed();
        return Err(UpdateError::Persist);
    }
    crate::todo_attachments::cleanup_attachments(&removed);
    Ok(updated)
}

pub fn set_auto_at(path: &Path, lock: &Path, project: &str, id: &str, auto: bool) -> Option<bool> {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let entry = data
        .projects
        .get_mut(project.trim())?
        .iter_mut()
        .find(|e| e.id == id)?;
    if entry.auto == auto {
        return Some(auto);
    }
    entry.auto = auto;
    store_at(path, data);
    Some(auto)
}

pub fn set_text_at(
    path: &Path,
    lock: &Path,
    project: &str,
    id: &str,
    text: &str,
) -> Option<String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return None;
    }
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let entry = data
        .projects
        .get_mut(project.trim())?
        .iter_mut()
        .find(|e| e.id == id)?;
    if entry.text == text {
        return Some(text);
    }
    entry.text = text.clone();
    if !store_at(path, data) {
        return None;
    }
    Some(text)
}

pub fn remove_at(path: &Path, lock: &Path, project: &str, id: &str) -> bool {
    remove_checked_at(path, lock, project, id).unwrap_or(false)
}

fn remove_checked_at(
    path: &Path,
    lock: &Path,
    project: &str,
    id: &str,
) -> Result<bool, MutationError> {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let Some(entries) = data.projects.get_mut(project.trim()) else {
        return Ok(false);
    };
    let before = entries.len();
    let removed_entries: Vec<TodoEntry> = entries.iter().filter(|e| e.id == id).cloned().collect();
    entries.retain(|e| e.id != id);
    let removed = entries.len() != before;
    if !removed {
        return Ok(false);
    }
    if !store_at(path, data) {
        return Err(MutationError::Persist);
    }
    for entry in removed_entries {
        crate::todo_attachments::cleanup_attachments(&entry.attachments);
    }
    Ok(true)
}

pub fn clear_at(path: &Path, lock: &Path, project: &str) -> usize {
    clear_checked_at(path, lock, project).unwrap_or(0)
}

fn clear_checked_at(path: &Path, lock: &Path, project: &str) -> Result<usize, MutationError> {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let removed_entries = data.projects.remove(project.trim()).unwrap_or_default();
    let removed = removed_entries.len();
    if removed == 0 {
        return Ok(0);
    }
    if !store_at(path, data) {
        return Err(MutationError::Persist);
    }
    for entry in removed_entries {
        crate::todo_attachments::cleanup_attachments(&entry.attachments);
    }
    Ok(removed)
}

pub fn clear_history_at(path: &Path, lock: &Path, project: &str) -> usize {
    clear_history_checked_at(path, lock, project).unwrap_or(0)
}

fn clear_history_checked_at(
    path: &Path,
    lock: &Path,
    project: &str,
) -> Result<usize, MutationError> {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let removed_entries = data.history.remove(project.trim()).unwrap_or_default();
    let removed = removed_entries.len();
    if removed == 0 {
        return Ok(0);
    }
    if !store_at(path, data) {
        return Err(MutationError::Persist);
    }
    for entry in removed_entries {
        crate::todo_attachments::cleanup_attachments(&entry.attachments);
    }
    Ok(removed)
}

pub fn reorder_at(path: &Path, lock: &Path, project: &str, ids: &[String]) -> bool {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let Some(entries) = data.projects.get_mut(project.trim()) else {
        return false;
    };
    let before: Vec<String> = entries.iter().map(|e| e.id.clone()).collect();
    let mut rest = std::mem::take(entries);
    let mut listed: Vec<TodoEntry> = Vec::with_capacity(rest.len());
    for id in ids {
        if let Some(pos) = rest.iter().position(|e| &e.id == id) {
            listed.push(rest.remove(pos));
        }
    }
    // `ids` 之外的条目（并发新增等）按原相对顺序压后。
    listed.append(&mut rest);
    let changed = !listed.iter().map(|e| &e.id).eq(before.iter());
    *entries = listed;
    if changed {
        store_at(path, data);
    }
    changed
}

pub fn take_at(
    path: &Path,
    lock: &Path,
    project: &str,
    ids: &[String],
    history_limit: usize,
) -> Vec<TodoEntry> {
    if ids.is_empty() {
        return Vec::new();
    }
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let Some(entries) = data.projects.get_mut(project.trim()) else {
        return Vec::new();
    };
    let mut taken = Vec::new();
    entries.retain(|e| {
        if ids.iter().any(|id| id == &e.id) {
            taken.push(e.clone());
            false
        } else {
            true
        }
    });
    if !taken.is_empty() {
        let mut cleanup_after_store: Vec<crate::todo_attachments::TodoAttachment> = Vec::new();
        // Record execution history (chronological append; trim oldest beyond the cap).
        if history_limit > 0 {
            let done_at = now_ms();
            let hist = data.history.entry(project.trim().to_string()).or_default();
            for e in &taken {
                hist.push(DoneTodoEntry {
                    id: e.id.clone(),
                    text: e.text.clone(),
                    created_at_ms: e.created_at_ms,
                    agent_kind: e.agent_kind.clone(),
                    done_at_ms: done_at,
                    attachments: e.attachments.clone(),
                });
            }
            if hist.len() > history_limit {
                let overflow = hist.len() - history_limit;
                for entry in hist.drain(..overflow) {
                    cleanup_after_store.extend(entry.attachments);
                }
            }
        } else {
            for entry in &taken {
                cleanup_after_store.extend(entry.attachments.clone());
            }
        }
        if store_at(path, data) {
            crate::todo_attachments::cleanup_attachments(&cleanup_after_store);
        }
    }
    taken
}

pub fn history_at(path: &Path, project: &str) -> Vec<DoneTodoEntry> {
    let mut entries = load_at(path)
        .history
        .get(project.trim())
        .cloned()
        .unwrap_or_default();
    entries.reverse(); // newest first
    entries
}

pub fn restore_at(path: &Path, lock: &Path, project: &str, id: &str) -> bool {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let Some(hist) = data.history.get_mut(project.trim()) else {
        return false;
    };
    let Some(pos) = hist.iter().position(|e| e.id == id) else {
        return false;
    };
    let done = hist.remove(pos);
    data.projects
        .entry(project.trim().to_string())
        .or_default()
        .push(TodoEntry {
            id: done.id,
            text: done.text,
            created_at_ms: done.created_at_ms,
            agent_kind: done.agent_kind,
            // 恢复为普通待办：带 auto 恢复会立刻重新触发自动链，违背「找回来看看」的意图。
            auto: false,
            attachments: done.attachments,
        });
    store_at(path, data);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempStore {
        dir: PathBuf,
    }

    impl TempStore {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("ah-todos-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
        fn file(&self) -> PathBuf {
            self.dir.join("todos.json")
        }
        fn lock(&self) -> PathBuf {
            self.dir.join("todos.lock")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn option_attachment_badge_uses_brackets_without_emoji() {
        let entry = TodoEntry {
            id: "todo-1".into(),
            text: "review".into(),
            created_at_ms: 1,
            agent_kind: None,
            auto: false,
            attachments: vec![crate::todo_attachments::TodoAttachment {
                id: "attachment-1".into(),
                name: "brief.md".into(),
                size: 1,
                is_image: false,
                source_path: "/tmp/brief.md".into(),
                path: "/tmp/brief.md".into(),
                storage: crate::todo_attachments::TodoAttachmentStorage::Reference,
                thumbnail_path: None,
                source_modified_ms: None,
            }],
        };
        assert_eq!(
            option_label(crate::i18n::Lang::Zh, &entry),
            "执行待办：review 【1 个附件】"
        );
        assert_eq!(
            option_label(crate::i18n::Lang::En, &entry),
            "Run todo: review 【1 attachments】"
        );
        assert!(!option_label(crate::i18n::Lang::Zh, &entry).contains('📎'));
        assert_eq!(
            split_attachment_badge("review 【1 个附件】"),
            ("review", Some("【1 个附件】"))
        );
        assert_eq!(
            split_attachment_badge("review 【user text】"),
            ("review 【user text】", None)
        );
    }

    #[test]
    fn add_list_fifo_roundtrip() {
        let t = TempStore::new();
        assert!(list_at(&t.file(), "/p").is_empty());
        let a = add_at(&t.file(), &t.lock(), "/p", "第一条").unwrap();
        let b = add_at(&t.file(), &t.lock(), "/p", "  second  ").unwrap();
        assert_eq!(b.text, "second"); // trimmed
        let entries = list_at(&t.file(), "/p");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, a.id); // FIFO order preserved
        assert_eq!(entries[0].text, "第一条");
        assert!(entries[0].created_at_ms > 0);
        // Other projects unaffected.
        assert!(list_at(&t.file(), "/q").is_empty());
    }

    #[test]
    fn agent_origin_round_trips_through_history_and_restore() {
        let t = TempStore::new();
        let agent = add_impl(
            &t.file(),
            &t.lock(),
            "/p",
            "agent task",
            false,
            Some("codex"),
        )
        .unwrap();
        let human = add_at(&t.file(), &t.lock(), "/p", "human task").unwrap();
        assert_eq!(agent.agent_kind.as_deref(), Some("codex"));
        assert_eq!(human.agent_kind, None);

        let raw = std::fs::read_to_string(t.file()).unwrap();
        assert_eq!(raw.matches("agentKind").count(), 1);
        take_at(
            &t.file(),
            &t.lock(),
            "/p",
            std::slice::from_ref(&agent.id),
            20,
        );
        assert_eq!(
            history_at(&t.file(), "/p")[0].agent_kind.as_deref(),
            Some("codex")
        );

        assert!(restore_at(&t.file(), &t.lock(), "/p", &agent.id));
        let restored = list_at(&t.file(), "/p")
            .into_iter()
            .find(|entry| entry.id == agent.id)
            .unwrap();
        assert_eq!(restored.agent_kind.as_deref(), Some("codex"));
    }

    #[test]
    fn add_rejects_empty_text_or_project() {
        let t = TempStore::new();
        assert_eq!(
            add_at(&t.file(), &t.lock(), "/p", "   "),
            Err(AddError::EmptyInput)
        );
        assert_eq!(
            add_at(&t.file(), &t.lock(), "  ", "task"),
            Err(AddError::EmptyInput)
        );
        assert!(!t.file().exists());
    }

    #[test]
    fn add_reports_persist_failure_when_path_unwritable() {
        // Parent path is a regular file → create_dir_all / write cannot succeed.
        let t = TempStore::new();
        let blocker = t.dir.join("not-a-dir");
        std::fs::write(&blocker, b"x").unwrap();
        let file = blocker.join("todos.json");
        let lock = t.dir.join("todos.lock");
        assert_eq!(
            add_at(&file, &lock, "/p", "should not land"),
            Err(AddError::Persist)
        );
        assert!(!file.exists());
    }

    #[test]
    fn remove_by_id_and_prune_empty_project() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "one").unwrap();
        assert!(remove_at(&t.file(), &t.lock(), "/p", &a.id));
        assert!(!remove_at(&t.file(), &t.lock(), "/p", &a.id)); // already gone
        assert!(list_at(&t.file(), "/p").is_empty());
        // Project key pruned from file.
        let raw = std::fs::read_to_string(t.file()).unwrap();
        assert!(!raw.contains("/p"));
    }

    #[test]
    fn set_text_updates_trim_keeps_id_and_rejects_empty() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "old").unwrap();
        let id = a.id.clone();
        let stored = set_text_at(&t.file(), &t.lock(), "/p", &id, "  new line\ntext  ").unwrap();
        assert_eq!(stored, "new line\ntext");
        let entries = list_at(&t.file(), "/p");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, id);
        assert_eq!(entries[0].text, "new line\ntext");
        assert_eq!(entries[0].created_at_ms, a.created_at_ms);
        assert!(set_text_at(&t.file(), &t.lock(), "/p", &id, "   ").is_none());
        assert_eq!(list_at(&t.file(), "/p")[0].text, "new line\ntext");
        assert!(set_text_at(&t.file(), &t.lock(), "/p", "missing", "x").is_none());
    }

    #[test]
    fn clear_returns_count_and_needs_entries() {
        let t = TempStore::new();
        let _ = add_at(&t.file(), &t.lock(), "/p", "a");
        let _ = add_at(&t.file(), &t.lock(), "/p", "b");
        assert_eq!(clear_at(&t.file(), &t.lock(), "/p"), 2);
        assert_eq!(clear_at(&t.file(), &t.lock(), "/p"), 0);
    }

    #[test]
    fn take_dequeues_best_effort() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "a").unwrap();
        let b = add_at(&t.file(), &t.lock(), "/p", "b").unwrap();
        // One real id + one stale id: only the real one is taken, no error (spec D11).
        let taken = take_at(
            &t.file(),
            &t.lock(),
            "/p",
            &[a.id.clone(), "missing".to_string()],
            20,
        );
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].text, "a");
        let left = list_at(&t.file(), "/p");
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].id, b.id);
        // Empty ids → no-op.
        assert!(take_at(&t.file(), &t.lock(), "/p", &[], 20).is_empty());
        // Unknown project → no-op.
        assert!(take_at(&t.file(), &t.lock(), "/q", std::slice::from_ref(&b.id), 20).is_empty());
    }

    #[test]
    fn take_records_history_and_restore_moves_back() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "a").unwrap();
        let b = add_at(&t.file(), &t.lock(), "/p", "b").unwrap();
        take_at(&t.file(), &t.lock(), "/p", std::slice::from_ref(&a.id), 20);
        take_at(&t.file(), &t.lock(), "/p", std::slice::from_ref(&b.id), 20);
        // 倒序（最新在前）；带完成时间。
        let hist = history_at(&t.file(), "/p");
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].id, b.id);
        assert_eq!(hist[1].id, a.id);
        assert!(hist[0].done_at_ms > 0);
        // 恢复：回到待办队列末尾并从历史移除。
        assert!(restore_at(&t.file(), &t.lock(), "/p", &a.id));
        assert!(!restore_at(&t.file(), &t.lock(), "/p", &a.id)); // already restored
        let entries = list_at(&t.file(), "/p");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, a.id);
        assert_eq!(entries[0].text, "a");
        assert_eq!(history_at(&t.file(), "/p").len(), 1);
        // 手动删除不进历史。
        remove_at(&t.file(), &t.lock(), "/p", &a.id);
        assert_eq!(history_at(&t.file(), "/p").len(), 1);
    }

    #[test]
    fn attachments_round_trip_through_history_and_restore() {
        let t = TempStore::new();
        let mut entry = add_at(&t.file(), &t.lock(), "/p", "with file").unwrap();
        entry
            .attachments
            .push(crate::todo_attachments::TodoAttachment {
                id: uuid::Uuid::new_v4().to_string(),
                name: "brief.md".into(),
                size: 10,
                is_image: false,
                source_path: "/external/brief.md".into(),
                path: "/external/brief.md".into(),
                storage: crate::todo_attachments::TodoAttachmentStorage::Reference,
                thumbnail_path: None,
                source_modified_ms: None,
            });
        let mut data = load_at(&t.file());
        data.projects.get_mut("/p").unwrap()[0] = entry.clone();
        assert!(store_at(&t.file(), data));

        let taken = take_at(&t.file(), &t.lock(), "/p", &[entry.id.clone()], 20);
        assert_eq!(taken[0].attachments, entry.attachments);
        assert_eq!(
            history_at(&t.file(), "/p")[0].attachments,
            entry.attachments
        );
        assert!(restore_at(&t.file(), &t.lock(), "/p", &entry.id));
        assert_eq!(list_at(&t.file(), "/p")[0].attachments, entry.attachments);
    }

    #[test]
    fn history_caps_at_limit_and_zero_disables_recording() {
        let t = TempStore::new();
        for i in 0..4 {
            let e = add_at(&t.file(), &t.lock(), "/p", &format!("t{i}")).unwrap();
            take_at(&t.file(), &t.lock(), "/p", &[e.id], 3);
        }
        // 超上限丢最旧：只剩最近 3 条（t1..t3，倒序 t3 在前）。
        let hist = history_at(&t.file(), "/p");
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].text, "t3");
        assert_eq!(hist[2].text, "t1");
        // limit=0：停止新增，但既有历史保留（与回复历史同语义）。
        let e = add_at(&t.file(), &t.lock(), "/p", "t4").unwrap();
        take_at(&t.file(), &t.lock(), "/p", &[e.id], 0);
        let hist = history_at(&t.file(), "/p");
        assert_eq!(hist.len(), 3);
        assert_eq!(hist[0].text, "t3");
    }

    #[test]
    fn ids_to_dequeue_collects_selected_chips_and_explicit_ids() {
        use crate::models::{
            AskRequest, ChannelAction, ChannelResult, MessagePrompt, OptionItem, Question,
            QuestionAnswer,
        };
        let request = AskRequest::new(
            MessagePrompt::default(),
            vec![Question::new(
                "What should we do next?".into(),
                vec![
                    OptionItem::with_todo("修 bug", "id-1"),
                    OptionItem::with_todo("写文档", "id-2"),
                    OptionItem::new("End this turn", false),
                ],
            )],
            true,
        );
        // 选中一条待办 chip + 弹窗折叠区显式 id（含重复）→ 去重合并；「结束」选项无 id。
        let result = ChannelResult {
            action: ChannelAction::Send,
            answers: vec![QuestionAnswer {
                selected_options: vec!["修 bug".into(), "End this turn".into()],
                user_input: None,
                images: Vec::new(),
                files: Vec::new(),
                todo_ids: vec!["id-1".into(), "id-3".into()],
                todo_selections: Vec::new(),
            }],
            source_channel_id: "popup".into(),
        };
        assert_eq!(ids_to_dequeue(&request, &result), vec!["id-1", "id-3"]);
        // 取消路径不出队。
        assert!(ids_to_dequeue(&request, &ChannelResult::cancel("popup")).is_empty());
        // 普通提问（无 todo 选项、无显式 id）不出队。
        let plain = ChannelResult {
            action: ChannelAction::Send,
            answers: vec![QuestionAnswer {
                selected_options: vec!["End this turn".into()],
                ..Default::default()
            }],
            source_channel_id: "popup".into(),
        };
        assert!(ids_to_dequeue(&request, &plain).is_empty());
    }

    #[test]
    fn selected_todo_delivers_snapshot_files_and_missing_warnings() {
        use crate::models::{
            AskRequest, ChannelAction, ChannelResult, MessagePrompt, OptionItem, Question,
            QuestionAnswer,
        };
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("brief.md");
        std::fs::write(&file, b"brief").unwrap();
        let attachment = crate::todo_attachments::TodoAttachment {
            id: uuid::Uuid::new_v4().to_string(),
            name: "brief.md".into(),
            size: 5,
            is_image: false,
            source_path: file.to_string_lossy().into_owned(),
            path: file.to_string_lossy().into_owned(),
            storage: crate::todo_attachments::TodoAttachmentStorage::Reference,
            thumbnail_path: None,
            source_modified_ms: None,
        };
        let entry = TodoEntry {
            id: uuid::Uuid::new_v4().to_string(),
            text: "review brief".into(),
            created_at_ms: 1,
            agent_kind: None,
            auto: false,
            attachments: vec![attachment],
        };
        let label = option_label(crate::i18n::Lang::En, &entry);
        let request = AskRequest::new(
            MessagePrompt::default(),
            vec![Question::new(
                "next?".into(),
                vec![OptionItem::with_todo_entry(label.clone(), &entry)],
            )],
            true,
        );
        let mut result = ChannelResult {
            action: ChannelAction::Send,
            answers: vec![QuestionAnswer {
                selected_options: vec![label.clone()],
                ..Default::default()
            }],
            source_channel_id: "popup".into(),
        };
        apply_todo_deliveries_from_entries(&request, &mut result, std::slice::from_ref(&entry));
        assert_eq!(result.answers[0].selected_options, vec![label]);
        assert_eq!(result.answers[0].files, vec![file.to_string_lossy()]);
        assert!(result.answers[0].user_input.is_none());

        let mut missing = ChannelResult {
            action: ChannelAction::Send,
            answers: vec![QuestionAnswer {
                selected_options: result.answers[0].selected_options.clone(),
                ..Default::default()
            }],
            source_channel_id: "popup".into(),
        };
        apply_todo_deliveries_from_entries(&request, &mut missing, &[]);
        assert!(missing.answers[0].files.is_empty());
        assert!(missing.answers[0]
            .user_input
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"));
    }

    #[test]
    fn clear_history_removes_project_history_only() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "a").unwrap();
        let _b = add_at(&t.file(), &t.lock(), "/p", "b").unwrap();
        take_at(&t.file(), &t.lock(), "/p", std::slice::from_ref(&a.id), 20);
        assert_eq!(history_at(&t.file(), "/p").len(), 1);
        assert_eq!(clear_history_at(&t.file(), &t.lock(), "/p"), 1);
        assert!(history_at(&t.file(), "/p").is_empty());
        // 待办队列不受影响；未知项目 → 0。
        assert_eq!(list_at(&t.file(), "/p").len(), 1);
        assert_eq!(clear_history_at(&t.file(), &t.lock(), "/q"), 0);
    }

    #[test]
    fn reorder_moves_listed_ids_and_keeps_unlisted_tail() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "a").unwrap();
        let b = add_at(&t.file(), &t.lock(), "/p", "b").unwrap();
        let c = add_at(&t.file(), &t.lock(), "/p", "c").unwrap();
        // 完整重排 b, c, a。
        assert!(reorder_at(
            &t.file(),
            &t.lock(),
            "/p",
            &[b.id.clone(), c.id.clone(), a.id.clone()]
        ));
        let ids: Vec<_> = list_at(&t.file(), "/p").into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![b.id.clone(), c.id.clone(), a.id.clone()]);
        // 相同顺序 → 未变化，不写盘。
        assert!(!reorder_at(
            &t.file(),
            &t.lock(),
            "/p",
            &[b.id.clone(), c.id.clone(), a.id.clone()]
        ));
        // 部分 id（含过期 id）：列出的排前，其余压后保持相对顺序；过期 id 忽略（best-effort）。
        assert!(reorder_at(
            &t.file(),
            &t.lock(),
            "/p",
            &[a.id.clone(), "missing".to_string()]
        ));
        let ids: Vec<_> = list_at(&t.file(), "/p").into_iter().map(|e| e.id).collect();
        assert_eq!(ids, vec![a.id.clone(), b.id.clone(), c.id.clone()]);
        // 未知项目 → no-op。
        assert!(!reorder_at(&t.file(), &t.lock(), "/q", &[a.id]));
    }

    #[test]
    fn overflow_note_only_above_cap() {
        use crate::i18n::Lang;
        assert!(overflow_note(MAX_OPTION_TODOS, Lang::Zh).is_none());
        let note = overflow_note(MAX_OPTION_TODOS + 3, Lang::Zh).unwrap();
        assert!(note.contains('3'), "{note}");
    }

    #[test]
    fn auto_flag_add_toggle_and_first_auto_order() {
        let t = TempStore::new();
        let a = add_at(&t.file(), &t.lock(), "/p", "normal").unwrap();
        let b = add_auto_at(&t.file(), &t.lock(), "/p", "auto1").unwrap();
        let c = add_auto_at(&t.file(), &t.lock(), "/p", "auto2").unwrap();
        assert!(!a.auto);
        assert!(b.auto && c.auto);
        // 最靠前的自动待办 = 队列顺序里第一条 auto（b 在 c 前）。
        let first = list_at(&t.file(), "/p")
            .into_iter()
            .find(|e| e.auto)
            .unwrap();
        assert_eq!(first.id, b.id);
        // 切换：关掉 b 后轮到 c；开回普通条目 a 后 a 最靠前。
        assert_eq!(
            set_auto_at(&t.file(), &t.lock(), "/p", &b.id, false),
            Some(false)
        );
        let first = list_at(&t.file(), "/p")
            .into_iter()
            .find(|e| e.auto)
            .unwrap();
        assert_eq!(first.id, c.id);
        assert_eq!(
            set_auto_at(&t.file(), &t.lock(), "/p", &a.id, true),
            Some(true)
        );
        let first = list_at(&t.file(), "/p")
            .into_iter()
            .find(|e| e.auto)
            .unwrap();
        assert_eq!(first.id, a.id);
        // 不存在的 id → None。
        assert_eq!(
            set_auto_at(&t.file(), &t.lock(), "/p", "missing", true),
            None
        );
        // 序列化：auto=false 不落盘（skip_serializing_if），旧文件无该字段可读。
        let raw = std::fs::read_to_string(t.file()).unwrap();
        assert_eq!(raw.matches("\"auto\"").count(), 2); // a 与 c

        // 恢复历史不带 auto。
        take_at(&t.file(), &t.lock(), "/p", std::slice::from_ref(&a.id), 20);
        assert!(restore_at(&t.file(), &t.lock(), "/p", &a.id));
        let restored = list_at(&t.file(), "/p")
            .into_iter()
            .find(|e| e.id == a.id)
            .unwrap();
        assert!(!restored.auto);
    }

    #[test]
    fn corrupt_file_degrades_to_empty() {
        let t = TempStore::new();
        std::fs::write(t.file(), "not json").unwrap();
        assert!(list_at(&t.file(), "/p").is_empty());
        // Mutation on top of a corrupt file starts fresh instead of failing.
        add_at(&t.file(), &t.lock(), "/p", "x").unwrap();
        assert_eq!(list_at(&t.file(), "/p").len(), 1);
    }

    #[test]
    fn all_snapshot_groups_by_project() {
        let t = TempStore::new();
        let _ = add_at(&t.file(), &t.lock(), "/p", "a");
        let _ = add_at(&t.file(), &t.lock(), "/q", "b");
        let all = load_at(&t.file()).projects;
        assert_eq!(all.len(), 2);
        assert_eq!(all["/p"][0].text, "a");
        assert_eq!(all["/q"][0].text, "b");
    }
}
