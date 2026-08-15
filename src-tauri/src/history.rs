//! Reply history store: append-only JSONL at `~/.askhuman/history.jsonl` (one entry per line).
//!
//! Recording is a best-effort side channel invoked from the per-request coordinator after a
//! terminal result is produced; it must never affect the main flow (stdout / exit code). Image and
//! reply-file values are stored as paths only (never base64); display is best-effort (a missing
//! file just renders a placeholder). Mutating ops hold a cross-process file lock. Rewrites use an
//! atomic temp-file rename, while full deletion removes the store. Reads tolerate malformed lines.

use crate::models::{ChannelAction, MessagePrompt, Question};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

/// One question's recorded answer (paths only, no base64).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryAnswer {
    #[serde(default)]
    pub selected_options: Vec<String>,
    #[serde(default)]
    pub user_input: Option<String>,
    /// Saved image file paths (already written to disk by `render_result`).
    #[serde(default)]
    pub images: Vec<String>,
    /// Reply file absolute paths (passed through, not copied).
    #[serde(default)]
    pub files: Vec<String>,
}

/// One recorded reply (one per request: the winning terminal result).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp_ms: i64,
    #[serde(default)]
    pub project: String,
    /// Caller source name (`ASKHUMAN_ENV_SOURCE_NAME`).
    #[serde(default)]
    pub source: String,
    /// Caller agent family (claude/codex/cursor/grok). None when undetected or for
    /// entries recorded before this field existed. For MCP-originated asks the env
    /// carries nothing; the daemon's async process-tree walk backfills the value
    /// before the entry is recorded (best-effort).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_kind: Option<String>,
    /// Native Agent conversation/session id. None for legacy, non-Agent, or calls where the MCP
    /// client could not provide a trustworthy per-call binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// Random id owned by one long-lived AskHuman MCP server process. This is only a best-effort
    /// fallback partition when no true Agent session id is available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_instance_id: Option<String>,
    /// Channel id that submitted / cancelled (popup / dingding / feishu / telegram).
    #[serde(default)]
    pub channel: String,
    pub action: ChannelAction,
    #[serde(default)]
    pub is_markdown: bool,
    #[serde(default)]
    pub message: MessagePrompt,
    #[serde(default)]
    pub questions: Vec<Question>,
    /// Per-question answers (empty for a cancel).
    #[serde(default)]
    pub answers: Vec<HistoryAnswer>,
}

/// Aggregated project info for the history window's project picker.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectInfo {
    pub key: String,
    pub name: String,
    pub count: usize,
    pub last_ms: i64,
}

/// Current unix time in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ===== Public API (locks + default file path) =====

/// Append one entry (only when `limit > 0`) then trim to the most recent `limit`. When `limit == 0`
/// no new entry is recorded, but existing entries are still trimmed to the limit (i.e. cleared) on
/// the same schedule as a positive limit. Best-effort: errors are ignored.
pub fn record(entry: HistoryEntry, limit: u32) {
    let _guard = lock();
    record_at(&paths::history_file(), entry, limit);
}

/// Load entries (optionally filtered to one project), most recent first.
pub fn load(project: Option<&str>, all: bool) -> Vec<HistoryEntry> {
    load_at(&paths::history_file(), project, all)
}

/// Latest completed exchange for one exact native Agent session.
pub fn latest_send_for_session(agent_kind: &str, session_id: &str) -> Option<HistoryEntry> {
    recent_sends_for_session(agent_kind, session_id, 1)
        .entries
        .into_iter()
        .next()
}

/// Latest completed exchange in one MCP-process/project fallback partition.
pub fn latest_send_for_mcp_instance(mcp_instance_id: &str, project: &str) -> Option<HistoryEntry> {
    recent_sends_for_mcp_instance(mcp_instance_id, project, 1)
        .entries
        .into_iter()
        .next()
}

/// Latest completed exchange for a project, used only by an ordinary non-Agent CLI call.
pub fn latest_send_for_project(project: &str) -> Option<HistoryEntry> {
    recent_sends_for_project(project, 1)
        .entries
        .into_iter()
        .next()
}

/// Recent completed Send entries for a scope: newest-first slice of length ≤ `n`, plus total.
#[derive(Debug, Clone)]
pub struct RecentSends {
    /// Newest first; length ≤ requested `n`.
    pub entries: Vec<HistoryEntry>,
    /// Total matching Send entries in the store for this partition.
    pub total: usize,
}

/// Most recent completed exchanges for one exact native Agent session.
pub fn recent_sends_for_session(agent_kind: &str, session_id: &str, n: usize) -> RecentSends {
    recent_sends_for_session_at(&paths::history_file(), agent_kind, session_id, n)
}

/// Most recent completed exchanges in one MCP-process/project fallback partition.
pub fn recent_sends_for_mcp_instance(
    mcp_instance_id: &str,
    project: &str,
    n: usize,
) -> RecentSends {
    recent_sends_for_mcp_instance_at(&paths::history_file(), mcp_instance_id, project, n)
}

/// Most recent completed exchanges for a project (non-Agent CLI fallback).
pub fn recent_sends_for_project(project: &str, n: usize) -> RecentSends {
    recent_sends_for_project_at(&paths::history_file(), project, n)
}

/// Count completed Send entries in a session with `timestamp_ms` strictly after `after_ms`.
pub fn count_sends_for_session_after(agent_kind: &str, session_id: &str, after_ms: i64) -> usize {
    count_sends_after_at(&paths::history_file(), after_ms, |entry| {
        entry.agent_kind.as_deref() == Some(agent_kind)
            && entry.agent_session_id.as_deref() == Some(session_id)
    })
}

/// Count completed Send entries for an MCP instance partition after `after_ms`.
pub fn count_sends_for_mcp_instance_after(
    mcp_instance_id: &str,
    project: &str,
    after_ms: i64,
) -> usize {
    count_sends_after_at(&paths::history_file(), after_ms, |entry| {
        entry.mcp_instance_id.as_deref() == Some(mcp_instance_id) && entry.project == project
    })
}

/// Count completed Send entries for a project after `after_ms`.
pub fn count_sends_for_project_after(project: &str, after_ms: i64) -> usize {
    count_sends_after_at(&paths::history_file(), after_ms, |entry| {
        entry.project == project
    })
}

/// Distinct projects present in history, most recently active first.
pub fn projects() -> Vec<ProjectInfo> {
    projects_at(&paths::history_file())
}

/// Total number of entries.
pub fn count() -> usize {
    read_all_at(&paths::history_file()).len()
}

/// Trim to the most recent `limit` entries (`limit == 0` clears all). Returns the remaining entry
/// count.
pub fn trim(limit: u32) -> usize {
    let _guard = lock();
    trim_at(&paths::history_file(), limit)
}

/// Delete the exact entry ids selected by the history UI. Unlike best-effort recording, this is a
/// user-initiated destructive operation and therefore reports lock/read/write failures.
pub fn delete_ids(ids: &[String]) -> io::Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let _guard = lock_strict()?;
    delete_ids_at(&paths::history_file(), ids)
}

/// Clear every valid history entry and return how many were removed.
pub fn clear_all() -> io::Result<usize> {
    let _guard = lock_strict()?;
    clear_all_at(&paths::history_file())
}

// ===== Core logic (path-parameterized, lock-free; unit-testable) =====

fn record_at(path: &Path, entry: HistoryEntry, limit: u32) {
    let mut entries = read_all_at(path);
    // limit == 0 stops recording new entries, but existing ones are still trimmed to the limit
    // (i.e. cleared) on the same schedule as a positive limit. Skip touching disk when there is
    // nothing to add and nothing to clear.
    if limit > 0 {
        entries.push(entry);
    } else if entries.is_empty() {
        return;
    }
    trim_vec(&mut entries, limit as usize);
    let _ = write_all_at(path, &entries);
}

fn load_at(path: &Path, project: Option<&str>, all: bool) -> Vec<HistoryEntry> {
    let mut entries = read_all_at(path);
    if !all {
        let key = project.unwrap_or("");
        entries.retain(|e| e.project == key);
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp_ms));
    entries
}

fn recent_sends_at(
    path: &Path,
    n: usize,
    predicate: impl Fn(&HistoryEntry) -> bool,
) -> RecentSends {
    let mut matching: Vec<HistoryEntry> = read_all_at(path)
        .into_iter()
        .filter(|entry| entry.action == ChannelAction::Send && predicate(entry))
        .collect();
    matching.sort_by_key(|entry| std::cmp::Reverse(entry.timestamp_ms));
    let total = matching.len();
    if n == 0 {
        return RecentSends {
            entries: Vec::new(),
            total,
        };
    }
    matching.truncate(n);
    RecentSends {
        entries: matching,
        total,
    }
}

fn recent_sends_for_session_at(
    path: &Path,
    agent_kind: &str,
    session_id: &str,
    n: usize,
) -> RecentSends {
    recent_sends_at(path, n, |entry| {
        entry.agent_kind.as_deref() == Some(agent_kind)
            && entry.agent_session_id.as_deref() == Some(session_id)
    })
}

fn recent_sends_for_mcp_instance_at(
    path: &Path,
    mcp_instance_id: &str,
    project: &str,
    n: usize,
) -> RecentSends {
    recent_sends_at(path, n, |entry| {
        entry.mcp_instance_id.as_deref() == Some(mcp_instance_id) && entry.project == project
    })
}

fn recent_sends_for_project_at(path: &Path, project: &str, n: usize) -> RecentSends {
    recent_sends_at(path, n, |entry| entry.project == project)
}

fn count_sends_after_at(
    path: &Path,
    after_ms: i64,
    predicate: impl Fn(&HistoryEntry) -> bool,
) -> usize {
    read_all_at(path)
        .into_iter()
        .filter(|entry| {
            entry.action == ChannelAction::Send && entry.timestamp_ms > after_ms && predicate(entry)
        })
        .count()
}

fn latest_send_for_session_at(
    path: &Path,
    agent_kind: &str,
    session_id: &str,
) -> Option<HistoryEntry> {
    recent_sends_for_session_at(path, agent_kind, session_id, 1)
        .entries
        .into_iter()
        .next()
}

fn latest_send_for_mcp_instance_at(
    path: &Path,
    mcp_instance_id: &str,
    project: &str,
) -> Option<HistoryEntry> {
    recent_sends_for_mcp_instance_at(path, mcp_instance_id, project, 1)
        .entries
        .into_iter()
        .next()
}

fn latest_send_for_project_at(path: &Path, project: &str) -> Option<HistoryEntry> {
    recent_sends_for_project_at(path, project, 1)
        .entries
        .into_iter()
        .next()
}

fn projects_at(path: &Path) -> Vec<ProjectInfo> {
    let entries = read_all_at(path);
    let mut map: HashMap<String, (usize, i64)> = HashMap::new();
    for e in &entries {
        let slot = map.entry(e.project.clone()).or_insert((0, 0));
        slot.0 += 1;
        if e.timestamp_ms > slot.1 {
            slot.1 = e.timestamp_ms;
        }
    }
    let mut out: Vec<ProjectInfo> = map
        .into_iter()
        .map(|(key, (count, last_ms))| ProjectInfo {
            name: crate::project::display_name(&key),
            key,
            count,
            last_ms,
        })
        .collect();
    out.sort_by_key(|p| std::cmp::Reverse(p.last_ms));
    out
}

fn trim_at(path: &Path, limit: u32) -> usize {
    let mut entries = read_all_at(path);
    trim_vec(&mut entries, limit as usize);
    let _ = write_all_at(path, &entries);
    entries.len()
}

fn delete_ids_at(path: &Path, ids: &[String]) -> io::Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let ids: HashSet<&str> = ids.iter().map(String::as_str).collect();
    let text = read_text_strict_at(path)?;
    let mut output = String::with_capacity(text.len());
    let mut deleted = 0;
    for line in text.split_inclusive('\n') {
        let json = line.strip_suffix('\n').unwrap_or(line);
        let json = json.strip_suffix('\r').unwrap_or(json);
        let matches = serde_json::from_str::<HistoryEntry>(json)
            .map(|entry| ids.contains(entry.id.as_str()))
            .unwrap_or(false);
        if matches {
            deleted += 1;
        } else {
            output.push_str(line);
        }
    }
    if deleted > 0 {
        write_bytes_at(path, output.as_bytes())?;
    }
    Ok(deleted)
}

fn clear_all_at(path: &Path) -> io::Result<usize> {
    let count = read_all_strict_at(path)?.len();
    match std::fs::remove_file(path) {
        Ok(()) => Ok(count),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

/// Keep only the most recent `limit` entries (entries are in append order: oldest first);
/// `limit == 0` drops them all.
fn trim_vec(entries: &mut Vec<HistoryEntry>, limit: usize) {
    if entries.len() > limit {
        let drop = entries.len() - limit;
        entries.drain(0..drop);
    }
}

fn read_all_at(path: &Path) -> Vec<HistoryEntry> {
    read_all_strict_at(path).unwrap_or_default()
}

fn read_all_strict_at(path: &Path) -> io::Result<Vec<HistoryEntry>> {
    let text = read_text_strict_at(path)?;
    Ok(text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<HistoryEntry>(l).ok())
        .collect())
}

fn read_text_strict_at(path: &Path) -> io::Result<String> {
    Ok(match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error),
    })
}

fn write_all_at(path: &Path, entries: &[HistoryEntry]) -> std::io::Result<()> {
    let mut buf = String::new();
    for e in entries {
        if let Ok(line) = serde_json::to_string(e) {
            buf.push_str(&line);
            buf.push('\n');
        }
    }
    write_bytes_at(path, buf.as_bytes())
}

fn write_bytes_at(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("jsonl.tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)?;
    harden(&tmp);
    std::fs::rename(&tmp, path)?;
    harden(path);
    Ok(())
}

/// Restrict the history file to owner read/write (0600) on Unix; no-op elsewhere.
#[cfg(unix)]
fn harden(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.permissions().mode() & 0o777 != 0o600 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}
#[cfg(not(unix))]
fn harden(_path: &Path) {}

// ===== Cross-process write lock =====

/// Acquire an exclusive (blocking) advisory lock for the duration of a write. Released on drop.
fn lock() -> Option<crate::file_lock::FileLock> {
    lock_strict().ok()
}

fn lock_strict() -> io::Result<crate::file_lock::FileLock> {
    crate::file_lock::FileLock::exclusive(&paths::history_lock())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(id: &str, project: &str, ts: i64) -> HistoryEntry {
        HistoryEntry {
            id: id.to_string(),
            timestamp_ms: ts,
            project: project.to_string(),
            source: "the Loop".to_string(),
            agent_kind: None,
            agent_session_id: None,
            mcp_instance_id: None,
            channel: "popup".to_string(),
            action: ChannelAction::Send,
            is_markdown: true,
            message: MessagePrompt::default(),
            questions: Vec::new(),
            answers: Vec::new(),
        }
    }

    #[test]
    fn roundtrip_and_order() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        record_at(&path, entry("a", "/p", 1), 200);
        record_at(&path, entry("b", "/p", 2), 200);
        let loaded = load_at(&path, Some("/p"), false);
        assert_eq!(loaded.len(), 2);
        // Most recent first.
        assert_eq!(loaded[0].id, "b");
        assert_eq!(loaded[1].id, "a");
    }

    #[test]
    fn agent_binding_fields_roundtrip_and_legacy_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let mut e = entry("a", "/p", 1);
        e.agent_kind = Some("cursor".to_string());
        e.agent_session_id = Some("conversation-1".to_string());
        e.mcp_instance_id = Some("instance-1".to_string());
        record_at(&path, e, 200);
        // Legacy line without the field parses with agent_kind == None.
        record_at(&path, entry("b", "/p", 2), 200);
        let loaded = load_at(&path, Some("/p"), false);
        assert_eq!(loaded[0].agent_kind, None);
        assert_eq!(loaded[0].agent_session_id, None);
        assert_eq!(loaded[0].mcp_instance_id, None);
        assert_eq!(loaded[1].agent_kind.as_deref(), Some("cursor"));
        assert_eq!(
            loaded[1].agent_session_id.as_deref(),
            Some("conversation-1")
        );
        assert_eq!(loaded[1].mcp_instance_id.as_deref(), Some("instance-1"));
        // None is omitted from the serialized line (keeps legacy shape).
        let raw = std::fs::read_to_string(&path).unwrap();
        assert_eq!(raw.matches("agentKind").count(), 1);
        assert_eq!(raw.matches("agentSessionId").count(), 1);
        assert_eq!(raw.matches("mcpInstanceId").count(), 1);
    }

    #[test]
    fn latest_send_queries_use_exact_partitions_and_ignore_cancel() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");

        let mut first = entry("first", "/p", 1);
        first.agent_kind = Some("codex".into());
        first.agent_session_id = Some("s1".into());
        first.mcp_instance_id = Some("m1".into());
        record_at(&path, first, 200);

        let mut cancelled = entry("cancelled", "/p", 2);
        cancelled.agent_kind = Some("codex".into());
        cancelled.agent_session_id = Some("s1".into());
        cancelled.mcp_instance_id = Some("m1".into());
        cancelled.action = ChannelAction::Cancel;
        record_at(&path, cancelled, 200);

        let mut other_session = entry("other", "/p", 3);
        other_session.agent_kind = Some("codex".into());
        other_session.agent_session_id = Some("s2".into());
        other_session.mcp_instance_id = Some("m1".into());
        record_at(&path, other_session, 200);

        assert_eq!(
            latest_send_for_session_at(&path, "codex", "s1").unwrap().id,
            "first"
        );
        assert_eq!(
            latest_send_for_mcp_instance_at(&path, "m1", "/p")
                .unwrap()
                .id,
            "other"
        );
        assert_eq!(latest_send_for_project_at(&path, "/p").unwrap().id, "other");

        // Exact Agent partitions include both kind and native session id.
        assert!(latest_send_for_session_at(&path, "cursor", "s1").is_none());
        assert!(latest_send_for_session_at(&path, "codex", "missing").is_none());
        // MCP fallback partitions include both process instance and project.
        assert!(latest_send_for_mcp_instance_at(&path, "m1", "/other").is_none());
        assert!(latest_send_for_mcp_instance_at(&path, "m2", "/p").is_none());
        assert!(latest_send_for_project_at(&path, "/other").is_none());

        // A restarted MCP process in the same project cannot see the previous instance partition.
        let mut restarted = entry("restarted", "/p", 4);
        restarted.mcp_instance_id = Some("m2".into());
        record_at(&path, restarted, 200);
        assert_eq!(
            latest_send_for_mcp_instance_at(&path, "m1", "/p")
                .unwrap()
                .id,
            "other"
        );
        assert_eq!(
            latest_send_for_mcp_instance_at(&path, "m2", "/p")
                .unwrap()
                .id,
            "restarted"
        );
    }

    #[test]
    fn recent_sends_return_newest_first_slice_and_total() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        for i in 1..=5 {
            let mut e = entry(&format!("e{i}"), "/p", i);
            e.agent_kind = Some("codex".into());
            e.agent_session_id = Some("s1".into());
            record_at(&path, e, 200);
        }
        let mut cancelled = entry("cancel", "/p", 6);
        cancelled.agent_kind = Some("codex".into());
        cancelled.agent_session_id = Some("s1".into());
        cancelled.action = ChannelAction::Cancel;
        record_at(&path, cancelled, 200);

        let recent = recent_sends_for_session_at(&path, "codex", "s1", 2);
        assert_eq!(recent.total, 5);
        assert_eq!(recent.entries.len(), 2);
        assert_eq!(recent.entries[0].id, "e5");
        assert_eq!(recent.entries[1].id, "e4");

        let all = recent_sends_for_session_at(&path, "codex", "s1", 100);
        assert_eq!(all.total, 5);
        assert_eq!(all.entries.len(), 5);
        assert_eq!(all.entries[4].id, "e1");
    }

    #[test]
    fn trim_keeps_most_recent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        for i in 0..5 {
            record_at(&path, entry(&format!("e{i}"), "/p", i), 3);
        }
        let loaded = load_at(&path, None, true);
        assert_eq!(loaded.len(), 3);
        // Kept e4,e3,e2 (desc).
        assert_eq!(loaded[0].id, "e4");
        assert_eq!(loaded[2].id, "e2");
    }

    #[test]
    fn limit_zero_records_nothing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        record_at(&path, entry("a", "/p", 1), 0);
        assert_eq!(read_all_at(&path).len(), 0);
    }

    #[test]
    fn limit_zero_clears_existing_but_adds_nothing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        record_at(&path, entry("a", "/p", 1), 200);
        record_at(&path, entry("b", "/p", 2), 200);
        assert_eq!(read_all_at(&path).len(), 2);
        // limit 0: the new entry is not added, and existing entries are trimmed to 0 (cleared).
        record_at(&path, entry("c", "/p", 3), 0);
        assert_eq!(read_all_at(&path).len(), 0);
    }

    #[test]
    fn filter_by_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        record_at(&path, entry("a", "/p1", 1), 200);
        record_at(&path, entry("b", "/p2", 2), 200);
        assert_eq!(load_at(&path, Some("/p1"), false).len(), 1);
        assert_eq!(load_at(&path, None, true).len(), 2);
        let projects = projects_at(&path);
        assert_eq!(projects.len(), 2);
        // Most recent project first.
        assert_eq!(projects[0].key, "/p2");
    }

    #[test]
    fn delete_ids_removes_only_the_frozen_snapshot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        record_at(&path, entry("a", "/p", 1), 200);
        record_at(&path, entry("b", "/p", 2), 200);
        let frozen = vec!["a".to_string(), "missing".to_string(), "a".to_string()];

        // A matching record arriving after the UI snapshot must survive exact-id deletion.
        record_at(&path, entry("new", "/p", 3), 200);
        assert_eq!(delete_ids_at(&path, &frozen).unwrap(), 1);
        let ids: Vec<String> = read_all_at(&path)
            .into_iter()
            .map(|entry| entry.id)
            .collect();
        assert_eq!(ids, vec!["b", "new"]);
    }

    #[test]
    fn delete_ids_empty_is_a_noop_and_clear_all_reports_count() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        record_at(&path, entry("a", "/p", 1), 200);
        assert_eq!(delete_ids_at(&path, &[]).unwrap(), 0);
        assert_eq!(read_all_at(&path).len(), 1);
        assert_eq!(clear_all_at(&path).unwrap(), 1);
        assert!(!path.exists());
        assert_eq!(clear_all_at(&path).unwrap(), 0);
    }

    #[test]
    fn destructive_reads_fail_closed() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::create_dir(&path).unwrap();
        assert!(delete_ids_at(&path, &["a".to_string()]).is_err());
        assert!(path.is_dir());
        assert!(clear_all_at(&path).is_err());
        assert!(path.is_dir());
    }

    #[test]
    fn exact_delete_preserves_malformed_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        let a = serde_json::to_string(&entry("a", "/p", 1)).unwrap();
        let b = serde_json::to_string(&entry("b", "/p", 2)).unwrap();
        std::fs::write(&path, format!("not json\n{a}\n{{}}\n{b}")).unwrap();

        assert_eq!(delete_ids_at(&path, &["a".to_string()]).unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            format!("not json\n{{}}\n{b}")
        );
    }

    #[test]
    fn skips_malformed_lines() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        std::fs::write(&path, "not json\n{}\n").unwrap();
        // Both lines fail to parse into a full entry (missing required fields) → skipped.
        assert_eq!(read_all_at(&path).len(), 0);
    }

    #[test]
    fn trim_zero_clears_all() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("history.jsonl");
        for i in 0..4 {
            record_at(&path, entry(&format!("e{i}"), "/p", i), 200);
        }
        // Trimming to a positive limit keeps the most recent N.
        assert_eq!(trim_at(&path, 2), 2);
        assert_eq!(read_all_at(&path).len(), 2);
        // Trimming to 0 clears everything.
        assert_eq!(trim_at(&path, 0), 0);
        assert_eq!(read_all_at(&path).len(), 0);
    }
}
