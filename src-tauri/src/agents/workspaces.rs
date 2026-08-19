//! Workspace index for IM-created Agent tasks.
//!
//! The initial index is recovered from recent local session metadata. This is a read-only scan:
//! it never starts an Agent and never writes into any Agent-owned directory.

use super::AgentKind;
use crate::paths;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_INDEXED: usize = 50;
const MAX_SCAN_FILES_PER_AGENT: usize = 300;
const MAX_JSONL_LINES: usize = 24;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub path: String,
    pub label: String,
    pub last_used_at: u64,
    #[serde(default)]
    pub agents: Vec<AgentKind>,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct WorkspaceFile {
    workspaces: Vec<Workspace>,
}

pub fn list() -> Vec<Workspace> {
    load().workspaces
}

/// Merge a bounded cold scan into the persistent index and return launchable entries.
pub fn refresh() -> Vec<Workspace> {
    let Ok(_lock) = WorkspaceLock::acquire() else {
        return list()
            .into_iter()
            .filter(|workspace| !workspace.hidden)
            .collect();
    };
    let mut by_path: HashMap<String, Workspace> = HashMap::new();
    for workspace in load().workspaces {
        let key = crate::path_identity::key(&workspace.path);
        if let Some(existing) = by_path.get_mut(&key) {
            merge_workspace(existing, workspace);
        } else {
            by_path.insert(key, workspace);
        }
    }
    for (path, kind, timestamp) in scan_recent() {
        let Ok(canonical) = fs::canonicalize(&path) else {
            continue;
        };
        if !canonical.is_dir() {
            continue;
        }
        let path = canonical.to_string_lossy().to_string();
        let key = crate::path_identity::key(&path);
        let entry = by_path.entry(key).or_insert_with(|| Workspace {
            label: workspace_label(&canonical),
            path,
            last_used_at: timestamp,
            agents: Vec::new(),
            pinned: false,
            hidden: false,
        });
        entry.last_used_at = entry.last_used_at.max(timestamp);
        if !entry.agents.contains(&kind) {
            entry.agents.push(kind);
        }
    }
    let mut workspaces: Vec<_> = by_path
        .into_values()
        .filter(|w| Path::new(&w.path).is_dir())
        .collect();
    sort_workspaces(&mut workspaces);
    workspaces.truncate(MAX_INDEXED);
    let _ = save(&WorkspaceFile {
        workspaces: workspaces.clone(),
    });
    workspaces.into_iter().filter(|w| !w.hidden).collect()
}

pub fn add(path: &Path, pinned: bool) -> Result<Workspace, String> {
    let _lock = WorkspaceLock::acquire().map_err(|e| e.to_string())?;
    let canonical = fs::canonicalize(path).map_err(|e| e.to_string())?;
    if !canonical.is_dir() {
        return Err("workspace is not a directory".to_string());
    }
    let path_text = canonical.to_string_lossy().to_string();
    let mut state = load();
    let now = epoch_secs(SystemTime::now());
    let value = if let Some(existing) = state
        .workspaces
        .iter_mut()
        .find(|w| crate::path_identity::equivalent(&w.path, &path_text))
    {
        existing.hidden = false;
        existing.pinned |= pinned;
        existing.last_used_at = existing.last_used_at.max(now);
        existing.clone()
    } else {
        let value = Workspace {
            path: path_text,
            label: workspace_label(&canonical),
            last_used_at: now,
            agents: Vec::new(),
            pinned,
            hidden: false,
        };
        state.workspaces.push(value.clone());
        value
    };
    sort_workspaces(&mut state.workspaces);
    state.workspaces.truncate(MAX_INDEXED);
    save(&state).map_err(|e| e.to_string())?;
    Ok(value)
}

pub fn set_pinned(path: &str, pinned: bool) -> Result<(), String> {
    let _lock = WorkspaceLock::acquire().map_err(|e| e.to_string())?;
    mutate(path, |w| w.pinned = pinned)
}

pub fn set_hidden(path: &str, hidden: bool) -> Result<(), String> {
    let _lock = WorkspaceLock::acquire().map_err(|e| e.to_string())?;
    mutate(path, |w| w.hidden = hidden)
}

pub fn forget(path: &str) -> Result<(), String> {
    let _lock = WorkspaceLock::acquire().map_err(|e| e.to_string())?;
    let mut state = load();
    state
        .workspaces
        .retain(|w| !crate::path_identity::equivalent(&w.path, path));
    save(&state).map_err(|e| e.to_string())
}

struct WorkspaceLock(crate::file_lock::FileLock);

impl WorkspaceLock {
    fn acquire() -> std::io::Result<Self> {
        crate::file_lock::FileLock::exclusive(&paths::agent_workspaces_lock()).map(Self)
    }
}

fn mutate(path: &str, f: impl FnOnce(&mut Workspace)) -> Result<(), String> {
    let mut state = load();
    let Some(item) = state
        .workspaces
        .iter_mut()
        .find(|w| crate::path_identity::equivalent(&w.path, path))
    else {
        return Err("workspace not found".to_string());
    };
    f(item);
    sort_workspaces(&mut state.workspaces);
    save(&state).map_err(|e| e.to_string())
}

fn load() -> WorkspaceFile {
    fs::read_to_string(paths::agent_workspaces_file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(state: &WorkspaceFile) -> std::io::Result<()> {
    let path = paths::agent_workspaces_file();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    fs::write(&tmp, serde_json::to_vec_pretty(state).unwrap_or_default())?;
    fs::rename(tmp, path)
}

fn sort_workspaces(items: &mut [Workspace]) {
    items.sort_by(|a, b| {
        b.pinned
            .cmp(&a.pinned)
            .then_with(|| b.last_used_at.cmp(&a.last_used_at))
            .then_with(|| {
                crate::path_identity::key(&a.path).cmp(&crate::path_identity::key(&b.path))
            })
    });
}

fn merge_workspace(existing: &mut Workspace, incoming: Workspace) {
    if incoming.last_used_at > existing.last_used_at {
        existing.path = incoming.path.clone();
        existing.label = incoming.label.clone();
    }
    existing.last_used_at = existing.last_used_at.max(incoming.last_used_at);
    existing.pinned |= incoming.pinned;
    existing.hidden &= incoming.hidden;
    for agent in incoming.agents {
        if !existing.agents.contains(&agent) {
            existing.agents.push(agent);
        }
    }
}

fn workspace_label(path: &Path) -> String {
    if let Some(label) = path
        .file_name()
        .and_then(|v| v.to_str())
        .filter(|v| !v.is_empty())
    {
        label.to_string()
    } else {
        path.to_string_lossy().to_string()
    }
}

fn scan_recent() -> Vec<(PathBuf, AgentKind, u64)> {
    let mut out = Vec::new();
    scan_jsonl(
        &paths::claude_dir().join("projects"),
        AgentKind::Claude,
        &mut out,
        |value| value.get("cwd").and_then(Value::as_str).map(PathBuf::from),
    );
    scan_jsonl(
        &paths::codex_dir().join("sessions"),
        AgentKind::Codex,
        &mut out,
        |value| {
            (value.get("type").and_then(Value::as_str) == Some("session_meta"))
                .then(|| value.pointer("/payload/cwd").and_then(Value::as_str))
                .flatten()
                .map(PathBuf::from)
        },
    );
    scan_grok(&mut out);
    scan_jsonl(
        &paths::pi_sessions_dir(),
        AgentKind::Pi,
        &mut out,
        |value| {
            (value.get("type").and_then(Value::as_str) == Some("session")
                && value.get("version").and_then(Value::as_u64) == Some(3))
            .then(|| value.get("cwd").and_then(Value::as_str))
            .flatten()
            .map(PathBuf::from)
        },
    );
    scan_cursor(&mut out);
    out
}

fn scan_jsonl(
    root: &Path,
    kind: AgentKind,
    out: &mut Vec<(PathBuf, AgentKind, u64)>,
    extract: impl Fn(&Value) -> Option<PathBuf>,
) {
    for (path, modified) in recent_files(root, Some("jsonl"), MAX_SCAN_FILES_PER_AGENT) {
        let Ok(file) = fs::File::open(path) else {
            continue;
        };
        for line in BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .take(MAX_JSONL_LINES)
        {
            let Some(cwd) = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|value| extract(&value))
            else {
                continue;
            };
            out.push((cwd, kind, modified));
            break;
        }
    }
}

fn scan_grok(out: &mut Vec<(PathBuf, AgentKind, u64)>) {
    let root = paths::home().join(".grok").join("sessions");
    for (path, modified) in recent_files(&root, Some("json"), MAX_SCAN_FILES_PER_AGENT) {
        if path.file_name().and_then(|v| v.to_str()) != Some("summary.json") {
            continue;
        }
        if let Some(cwd) = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| {
                value
                    .pointer("/info/cwd")
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
        {
            out.push((cwd, AgentKind::Grok, modified));
        }
    }
}

fn scan_cursor(out: &mut Vec<(PathBuf, AgentKind, u64)>) {
    let root = paths::cursor_dir().join("projects");
    let mut seen = HashSet::new();
    for (path, modified) in super::cursor_vscdb::recent_workspaces() {
        if seen.insert(path.clone()) {
            out.push((path, AgentKind::Cursor, modified));
        }
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten().take(MAX_SCAN_FILES_PER_AGENT) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if !kind.is_dir() {
            continue;
        }
        let encoded = entry.file_name().to_string_lossy().to_string();
        let matches = recover_cursor_project_key(&encoded);
        if matches.len() != 1 {
            continue;
        }
        let path = matches[0].clone();
        if !seen.insert(path.clone()) {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(epoch_secs)
            .unwrap_or(0);
        out.push((path, AgentKind::Cursor, modified));
    }
}

#[cfg(not(windows))]
fn recover_cursor_project_key(encoded: &str) -> Vec<PathBuf> {
    recover_cursor_path(encoded, Path::new("/"), 0, 2)
}

#[cfg(windows)]
fn recover_cursor_project_key(encoded: &str) -> Vec<PathBuf> {
    let Some((root, remainder)) = windows_cursor_key_root(encoded) else {
        return Vec::new();
    };
    recover_cursor_path(remainder, Path::new(&root), 0, 2)
}

/// Parse the unambiguous Windows key prefixes Cursor emits for drive paths. An explicit `UNC`
/// prefix is also accepted for forward compatibility; current UNC workspaces are recovered from
/// Cursor's exact database index before this lossy-key fallback is considered.
fn windows_cursor_key_root(encoded: &str) -> Option<(String, &str)> {
    let mut parts = encoded.splitn(2, '-');
    let first = parts.next()?;
    let remainder = parts.next().unwrap_or("");
    if first.len() == 1 && first.as_bytes()[0].is_ascii_alphabetic() {
        return Some((format!("{}:\\", first), remainder));
    }
    if first.eq_ignore_ascii_case("unc") {
        let mut unc = remainder.splitn(3, '-');
        let server = unc.next().filter(|part| !part.is_empty())?;
        let share = unc.next().filter(|part| !part.is_empty())?;
        let remainder = unc.next().unwrap_or("");
        return Some((format!("\\\\{server}\\{share}"), remainder));
    }
    None
}

/// Resolve Cursor's hyphen-joined project key against the real filesystem. Exploration stops as
/// soon as more than one candidate exists; ambiguous keys are intentionally ignored.
///
/// Probes candidate prefixes with a direct stat instead of enumerating directories: the only
/// entry names that can match `encoded` are its hyphen-boundary prefixes, so the match set is
/// identical to a `read_dir` scan. Crucially, `readdir` on macOS TCC-protected folders
/// (~/Desktop, ~/Documents, ~/Downloads, iCloud Drive) triggers a consent prompt per folder,
/// while stat of a known path does not — keys like `Users-me-Downloads-proj` used to make the
/// settings window (and the daemon) pop several permission dialogs.
fn recover_cursor_path(encoded: &str, base: &Path, depth: usize, limit: usize) -> Vec<PathBuf> {
    if encoded.is_empty() {
        return base
            .is_dir()
            .then(|| base.to_path_buf())
            .into_iter()
            .collect();
    }
    if depth > 64 || limit == 0 {
        return Vec::new();
    }
    // Every hyphen is a potential path separator; the whole key is the final candidate.
    let candidates = encoded
        .char_indices()
        .filter(|&(_, c)| c == '-')
        .map(|(i, _)| (&encoded[..i], &encoded[i + 1..]))
        .chain(std::iter::once((encoded, "")));
    let mut out = Vec::new();
    for (name, remainder) in candidates {
        if name.is_empty() {
            continue;
        }
        let path = base.join(name);
        if !path.is_dir() {
            continue;
        }
        let remaining_limit = limit.saturating_sub(out.len());
        out.extend(recover_cursor_path(
            remainder,
            &path,
            depth + 1,
            remaining_limit,
        ));
        if out.len() >= limit {
            break;
        }
    }
    out
}

fn recent_files(root: &Path, extension: Option<&str>, max: usize) -> Vec<(PathBuf, u64)> {
    let mut stack = vec![root.to_path_buf()];
    let mut files = Vec::new();
    let mut visited = 0usize;
    let visit_limit = max.saturating_mul(50).max(max);
    while let Some(dir) = stack.pop() {
        if visited >= visit_limit {
            break;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if visited > visit_limit {
                break;
            }
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if stack.len() < 2048 {
                    stack.push(path);
                }
            } else if kind.is_file()
                && extension
                    .is_none_or(|ext| path.extension().and_then(|v| v.to_str()) == Some(ext))
            {
                let modified = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(epoch_secs)
                    .unwrap_or(0);
                files.push((path, modified));
            }
        }
    }
    files.sort_by_key(|(_, modified)| std::cmp::Reverse(*modified));
    files.truncate(max);
    files
}

fn epoch_secs(value: SystemTime) -> u64 {
    value
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sort_prefers_pinned_then_recent() {
        let mut values = vec![
            Workspace {
                path: "/b".into(),
                label: "b".into(),
                last_used_at: 9,
                agents: vec![],
                pinned: false,
                hidden: false,
            },
            Workspace {
                path: "/a".into(),
                label: "a".into(),
                last_used_at: 1,
                agents: vec![],
                pinned: true,
                hidden: false,
            },
        ];
        sort_workspaces(&mut values);
        assert_eq!(values[0].path, "/a");
    }

    #[test]
    fn duplicate_workspace_metadata_is_merged_without_loss() {
        let mut existing = Workspace {
            path: r"C:\Work\Repo".into(),
            label: "old".into(),
            last_used_at: 1,
            agents: vec![AgentKind::Claude],
            pinned: true,
            hidden: true,
        };
        merge_workspace(
            &mut existing,
            Workspace {
                path: "c:/work/repo".into(),
                label: "new".into(),
                last_used_at: 2,
                agents: vec![AgentKind::Codex],
                pinned: false,
                hidden: false,
            },
        );

        assert_eq!(existing.path, "c:/work/repo");
        assert_eq!(existing.label, "new");
        assert_eq!(existing.last_used_at, 2);
        assert!(existing.pinned);
        assert!(!existing.hidden);
        assert_eq!(existing.agents, vec![AgentKind::Claude, AgentKind::Codex]);
    }

    #[test]
    fn cursor_recovery_requires_unique_existing_path() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("one").join("two-three")).unwrap();
        let root = dir.path().ancestors().last().unwrap();
        let encoded = format!(
            "{}-one-two-three",
            dir.path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace(['/', '\\'], "-")
        );
        let matches = recover_cursor_path(&encoded, root, 0, 2);
        assert_eq!(matches, vec![dir.path().join("one").join("two-three")]);
    }

    #[test]
    fn cursor_recovery_reports_ambiguous_keys() {
        let dir = tempfile::tempdir().unwrap();
        // Both interpretations of "one-two" exist: the nested one/two and the hyphenated one-two.
        fs::create_dir_all(dir.path().join("one").join("two")).unwrap();
        fs::create_dir_all(dir.path().join("one-two")).unwrap();
        let root = dir.path().ancestors().last().unwrap();
        let encoded = format!(
            "{}-one-two",
            dir.path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace(['/', '\\'], "-")
        );
        let matches = recover_cursor_path(&encoded, root, 0, 2);
        assert_eq!(matches.len(), 2, "ambiguity must surface both candidates");
    }

    #[test]
    fn windows_cursor_key_roots_preserve_drive_and_unc_identity() {
        assert_eq!(
            windows_cursor_key_root("C-Users-Alice-Repo"),
            Some(("C:\\".to_string(), "Users-Alice-Repo"))
        );
        assert_eq!(
            windows_cursor_key_root("e-project-中文-Space Here"),
            Some(("e:\\".to_string(), "project-中文-Space Here"))
        );
        assert_eq!(
            windows_cursor_key_root("UNC-server-share-Team-Repo"),
            Some(("\\\\server\\share".to_string(), "Team-Repo"))
        );
        assert_eq!(windows_cursor_key_root("Users-Alice-Repo"), None);
    }
}
