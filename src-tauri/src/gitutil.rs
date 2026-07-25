//! Git helpers for IM `/diff` and `/stage` (spec `docs/specs/im-diff-stage-transcript.md`).
//!
//! Unstaged-only (working tree vs index), plus untracked files. Staged-only changes are ignored.

use crate::project;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Max total diff body lines across all files before truncation.
/// (GitHub soft-collapses large PRs; we keep a hard line budget for IM attachments.)
pub const MAX_DIFF_LINES: usize = 3000;
/// Max lines embedded **per file** (beyond this: skip content, show “large file”).
pub const MAX_LINES_PER_FILE: usize = 400;
/// Max bytes to embed for a single file body (untracked full text or raw read).
/// GUI clients (Fork/SourceTree/VS Code) typically hide multi‑MB blobs; 48 KiB keeps
/// attachments readable. Files larger than this get a skip marker, not full content.
pub const MAX_FILE_BYTES: u64 = 48 * 1024;
/// If a tracked file’s working-tree size exceeds this, skip content even if git
/// produces a text diff (huge generated JSON/minified assets).
pub const MAX_TRACKED_FILE_BYTES: u64 = 256 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Modified,
    Deleted,
    Untracked,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Equal,
    Insert,
    Delete,
    Header,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    pub kind: FileChangeKind,
    pub lines: Vec<DiffLine>,
    /// True when body was omitted (binary / too large / truncated by global budget).
    pub skipped: bool,
    pub skip_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffModel {
    pub git_root: PathBuf,
    pub files: Vec<FileDiff>,
    pub truncated: bool,
    pub total_paths: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePreview {
    pub git_root: PathBuf,
    /// Paths that would be staged (relative to git root), sorted.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageResult {
    pub paths: Vec<String>,
}

/// Fingerprint of a path list for confirm-card staleness checks.
pub fn paths_fingerprint(paths: &[String]) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut sorted: Vec<&str> = paths.iter().map(|s| s.as_str()).collect();
    sorted.sort_unstable();
    let mut h = DefaultHasher::new();
    for p in sorted {
        p.hash(&mut h);
    }
    format!("{:x}", h.finish())
}

pub fn find_git_root(cwd: &Path) -> Option<PathBuf> {
    project::git_root(cwd)
}

/// List paths that `/diff` / `/stage` care about: unstaged modifications/deletes + untracked.
pub fn list_unstaged_paths(root: &Path) -> Result<Vec<String>, String> {
    let out = git(root, &["status", "--porcelain=v1", "-uall"])?;
    let mut paths = Vec::new();
    for line in out.lines() {
        if line.len() < 4 {
            continue;
        }
        let x = line.as_bytes()[0] as char;
        let y = line.as_bytes()[1] as char;
        // Rename: "R  old -> new" / "RM old -> new" etc. Take the new path after " -> ".
        let path_part = &line[3..];
        let rel = if let Some(i) = path_part.find(" -> ") {
            path_part[i + 4..].trim().to_string()
        } else {
            // Quoted paths: strip surrounding quotes best-effort.
            let t = path_part.trim();
            if t.starts_with('"') && t.ends_with('"') && t.len() >= 2 {
                t[1..t.len() - 1].to_string()
            } else {
                t.to_string()
            }
        };
        if rel.is_empty() {
            continue;
        }
        // Untracked.
        if x == '?' && y == '?' {
            paths.push(rel);
            continue;
        }
        // Working tree vs index: Y is not space → unstaged change.
        // Also include when deleted in worktree (Y == 'D') etc.
        if y != ' ' && y != '?' {
            paths.push(rel);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

pub fn preview_stage(root: &Path) -> Result<StagePreview, String> {
    let paths = list_unstaged_paths(root)?;
    Ok(StagePreview {
        git_root: root.to_path_buf(),
        paths,
    })
}

pub fn stage_all(root: &Path) -> Result<StageResult, String> {
    let paths = list_unstaged_paths(root)?;
    if paths.is_empty() {
        return Ok(StageResult { paths });
    }
    git(root, &["add", "-A"])?;
    Ok(StageResult { paths })
}

/// Stage only the given paths（spec gui-agent-console C15）。`paths` 必须来自本进程
/// `diff_stat`/`list_unstaged_paths` 的输出：这里再按当前未暂存清单白名单过滤一次
/// （不接受自由输入，杜绝把任意参数传给 git）。
pub fn stage_paths(root: &Path, paths: &[String]) -> Result<StageResult, String> {
    let known = list_unstaged_paths(root)?;
    let selected: Vec<String> = paths
        .iter()
        .filter(|p| known.iter().any(|k| k == *p))
        .cloned()
        .collect();
    if selected.is_empty() {
        return Ok(StageResult { paths: selected });
    }
    let mut args: Vec<&str> = vec!["add", "--"];
    for p in &selected {
        args.push(p.as_str());
    }
    git(root, &args)?;
    Ok(StageResult { paths: selected })
}

/// 一个文件的轻量统计（控制台 diff 状态条第一级，spec gui-agent-console C15/C16）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    /// git 根相对路径。
    pub path: String,
    /// `M` 修改 / `D` 工作树删除 / `A` 未跟踪新增 / `B` 二进制。
    pub kind: String,
    pub adds: usize,
    pub dels: usize,
}

/// 未暂存变更的轻量统计：一次 `git status` + 一次 `git diff --numstat`（未跟踪文件按
/// 有界读取数行）。不产出 hunk 正文——第二级按文件懒加载见 `diff_file`（C16 性能约束）。
pub fn diff_stat(root: &Path) -> Result<Vec<FileStat>, String> {
    let paths = list_unstaged_paths(root)?;
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let numstat = git(root, &["diff", "--numstat", "--no-color"]).unwrap_or_default();
    let mut tracked: std::collections::HashMap<String, (i64, i64)> =
        std::collections::HashMap::new();
    for line in numstat.lines() {
        let mut it = line.split('\t');
        let (Some(a), Some(d), Some(p)) = (it.next(), it.next(), it.next()) else {
            continue;
        };
        let adds = a.trim().parse::<i64>().unwrap_or(-1);
        let dels = d.trim().parse::<i64>().unwrap_or(-1);
        tracked.insert(p.trim().to_string(), (adds, dels));
    }
    let mut out = Vec::new();
    for rel in paths {
        let abs = root.join(&rel);
        if let Some((a, d)) = tracked.get(&rel) {
            let binary = *a < 0 || *d < 0;
            let deleted = !abs.exists();
            let kind = if binary {
                "B"
            } else if deleted {
                "D"
            } else {
                "M"
            };
            out.push(FileStat {
                path: rel,
                kind: kind.to_string(),
                adds: (*a).max(0) as usize,
                dels: (*d).max(0) as usize,
            });
        } else {
            // 未跟踪：有界读行数（二进制 / 超限 → 0 行）。
            let (adds, kind) = if is_probably_binary(&abs) {
                (0, "B")
            } else {
                match read_text_limited(&abs, MAX_FILE_BYTES) {
                    Ok(Some(text)) => (text.lines().count(), "A"),
                    _ => (0, "A"),
                }
            };
            out.push(FileStat {
                path: rel,
                kind: kind.to_string(),
                adds,
                dels: 0,
            });
        }
    }
    Ok(out)
}

/// 单文件 hunk 视图（控制台第二级懒加载）：只对该路径跑 `git diff` / 读未跟踪正文。
/// `rel` 应来自本进程 `diff_stat` 的输出（调用方约束）；未知路径产出空 diff。
pub fn diff_file(root: &Path, rel: &str) -> Result<FileDiff, String> {
    let mut budget = MAX_LINES_PER_FILE;
    Ok(diff_one(root, rel, &mut budget))
}

pub fn build_diff_model(root: &Path) -> Result<DiffModel, String> {
    let paths = list_unstaged_paths(root)?;
    let total_paths = paths.len();
    let mut files = Vec::new();
    let mut line_budget = MAX_DIFF_LINES;
    let mut truncated = false;

    for rel in &paths {
        if line_budget == 0 {
            truncated = true;
            files.push(FileDiff {
                path: rel.clone(),
                kind: FileChangeKind::Modified,
                lines: Vec::new(),
                skipped: true,
                skip_reason: Some("truncated".into()),
            });
            continue;
        }

        let abs = root.join(rel);
        let is_untracked = !path_is_tracked(root, rel);

        if is_untracked {
            if is_probably_binary(&abs) {
                files.push(FileDiff {
                    path: rel.clone(),
                    kind: FileChangeKind::Binary,
                    lines: Vec::new(),
                    skipped: true,
                    skip_reason: Some("binary".into()),
                });
                continue;
            }
            match read_text_limited(&abs, MAX_FILE_BYTES) {
                Ok(None) => {
                    files.push(FileDiff {
                        path: rel.clone(),
                        kind: FileChangeKind::Untracked,
                        lines: Vec::new(),
                        skipped: true,
                        skip_reason: Some("too_large".into()),
                    });
                }
                Ok(Some(text)) => {
                    let mut lines = Vec::new();
                    for l in text.lines() {
                        if line_budget == 0 {
                            truncated = true;
                            break;
                        }
                        lines.push(DiffLine {
                            kind: LineKind::Insert,
                            text: l.to_string(),
                        });
                        line_budget -= 1;
                    }
                    files.push(FileDiff {
                        path: rel.clone(),
                        kind: FileChangeKind::Untracked,
                        lines,
                        skipped: truncated && line_budget == 0,
                        skip_reason: if line_budget == 0 {
                            Some("truncated".into())
                        } else {
                            None
                        },
                    });
                }
                Err(_) => {
                    files.push(FileDiff {
                        path: rel.clone(),
                        kind: FileChangeKind::Untracked,
                        lines: Vec::new(),
                        skipped: true,
                        skip_reason: Some("unreadable".into()),
                    });
                }
            }
            continue;
        }

        // Tracked: skip huge working-tree files (templates/minified assets).
        if abs.is_file() {
            if let Ok(meta) = std::fs::metadata(&abs) {
                if meta.len() > MAX_TRACKED_FILE_BYTES {
                    files.push(FileDiff {
                        path: rel.clone(),
                        kind: FileChangeKind::Modified,
                        lines: Vec::new(),
                        skipped: true,
                        skip_reason: Some(format!("large file ({} KB)", meta.len() / 1024)),
                    });
                    continue;
                }
            }
        }

        // Tracked: use git diff (worktree vs index).
        let raw = git(root, &["diff", "--no-color", "--", rel]).unwrap_or_default();
        if raw.is_empty() {
            let deleted = !abs.exists();
            files.push(FileDiff {
                path: rel.clone(),
                kind: if deleted {
                    FileChangeKind::Deleted
                } else {
                    FileChangeKind::Modified
                },
                lines: Vec::new(),
                skipped: deleted,
                skip_reason: if deleted {
                    Some("deleted".into())
                } else {
                    None
                },
            });
            continue;
        }
        if raw.contains("Binary files ") || raw.contains("GIT binary patch") {
            files.push(FileDiff {
                path: rel.clone(),
                kind: FileChangeKind::Binary,
                lines: Vec::new(),
                skipped: true,
                skip_reason: Some("binary".into()),
            });
            continue;
        }
        // Huge patch text even if file is smaller (e.g. reformat).
        if raw.len() as u64 > MAX_FILE_BYTES * 4 {
            files.push(FileDiff {
                path: rel.clone(),
                kind: FileChangeKind::Modified,
                lines: Vec::new(),
                skipped: true,
                skip_reason: Some(format!("large diff ({} KB)", raw.len() / 1024)),
            });
            continue;
        }

        let mut lines = Vec::new();
        let mut file_truncated = false;
        for (file_lines, l) in raw.lines().enumerate() {
            if line_budget == 0 {
                truncated = true;
                break;
            }
            if file_lines >= MAX_LINES_PER_FILE {
                file_truncated = true;
                truncated = true;
                break;
            }
            let (kind, text) = if l.starts_with("+++")
                || l.starts_with("---")
                || l.starts_with("diff ")
                || l.starts_with("index ")
                || l.starts_with("@@")
            {
                (LineKind::Header, l.to_string())
            } else if let Some(rest) = l.strip_prefix('+') {
                (LineKind::Insert, rest.to_string())
            } else if let Some(rest) = l.strip_prefix('-') {
                (LineKind::Delete, rest.to_string())
            } else if let Some(rest) = l.strip_prefix(' ') {
                (LineKind::Equal, rest.to_string())
            } else {
                (LineKind::Header, l.to_string())
            };
            lines.push(DiffLine { kind, text });
            line_budget = line_budget.saturating_sub(1);
        }
        files.push(FileDiff {
            path: rel.clone(),
            kind: FileChangeKind::Modified,
            lines,
            skipped: file_truncated,
            skip_reason: if file_truncated {
                Some(format!("truncated after {MAX_LINES_PER_FILE} lines"))
            } else {
                None
            },
        });
    }

    Ok(DiffModel {
        git_root: root.to_path_buf(),
        files,
        truncated,
        total_paths,
    })
}

/// 单文件 diff 主体（与 `build_diff_model` 的逐文件逻辑同口径，但独立预算、
/// 不影响 IM `/diff` 的全局截断语义）。
fn diff_one(root: &Path, rel: &str, line_budget: &mut usize) -> FileDiff {
    let abs = root.join(rel);
    let is_untracked = !path_is_tracked(root, rel);

    if is_untracked {
        if is_probably_binary(&abs) {
            return FileDiff {
                path: rel.to_string(),
                kind: FileChangeKind::Binary,
                lines: Vec::new(),
                skipped: true,
                skip_reason: Some("binary".into()),
            };
        }
        return match read_text_limited(&abs, MAX_FILE_BYTES) {
            Ok(None) => FileDiff {
                path: rel.to_string(),
                kind: FileChangeKind::Untracked,
                lines: Vec::new(),
                skipped: true,
                skip_reason: Some("too_large".into()),
            },
            Ok(Some(text)) => {
                let mut lines = Vec::new();
                let mut truncated = false;
                for l in text.lines() {
                    if *line_budget == 0 {
                        truncated = true;
                        break;
                    }
                    lines.push(DiffLine {
                        kind: LineKind::Insert,
                        text: l.to_string(),
                    });
                    *line_budget -= 1;
                }
                FileDiff {
                    path: rel.to_string(),
                    kind: FileChangeKind::Untracked,
                    lines,
                    skipped: truncated,
                    skip_reason: truncated.then(|| "truncated".to_string()),
                }
            }
            Err(_) => FileDiff {
                path: rel.to_string(),
                kind: FileChangeKind::Untracked,
                lines: Vec::new(),
                skipped: true,
                skip_reason: Some("unreadable".into()),
            },
        };
    }

    // 已跟踪：超大工作树文件跳过内容。
    if abs.is_file() {
        if let Ok(meta) = std::fs::metadata(&abs) {
            if meta.len() > MAX_TRACKED_FILE_BYTES {
                return FileDiff {
                    path: rel.to_string(),
                    kind: FileChangeKind::Modified,
                    lines: Vec::new(),
                    skipped: true,
                    skip_reason: Some(format!("large file ({} KB)", meta.len() / 1024)),
                };
            }
        }
    }

    let raw = git(root, &["diff", "--no-color", "--", rel]).unwrap_or_default();
    if raw.is_empty() {
        let deleted = !abs.exists();
        return FileDiff {
            path: rel.to_string(),
            kind: if deleted {
                FileChangeKind::Deleted
            } else {
                FileChangeKind::Modified
            },
            lines: Vec::new(),
            skipped: deleted,
            skip_reason: deleted.then(|| "deleted".to_string()),
        };
    }
    if raw.contains("Binary files ") || raw.contains("GIT binary patch") {
        return FileDiff {
            path: rel.to_string(),
            kind: FileChangeKind::Binary,
            lines: Vec::new(),
            skipped: true,
            skip_reason: Some("binary".into()),
        };
    }
    if raw.len() as u64 > MAX_FILE_BYTES * 4 {
        return FileDiff {
            path: rel.to_string(),
            kind: FileChangeKind::Modified,
            lines: Vec::new(),
            skipped: true,
            skip_reason: Some(format!("large diff ({} KB)", raw.len() / 1024)),
        };
    }

    let mut lines = Vec::new();
    let mut file_truncated = false;
    for (file_lines, l) in raw.lines().enumerate() {
        if *line_budget == 0 || file_lines >= MAX_LINES_PER_FILE {
            file_truncated = true;
            break;
        }
        let (kind, text) = if l.starts_with("+++")
            || l.starts_with("---")
            || l.starts_with("diff ")
            || l.starts_with("index ")
            || l.starts_with("@@")
        {
            (LineKind::Header, l.to_string())
        } else if let Some(rest) = l.strip_prefix('+') {
            (LineKind::Insert, rest.to_string())
        } else if let Some(rest) = l.strip_prefix('-') {
            (LineKind::Delete, rest.to_string())
        } else if let Some(rest) = l.strip_prefix(' ') {
            (LineKind::Equal, rest.to_string())
        } else {
            (LineKind::Header, l.to_string())
        };
        lines.push(DiffLine { kind, text });
        *line_budget = line_budget.saturating_sub(1);
    }
    FileDiff {
        path: rel.to_string(),
        kind: FileChangeKind::Modified,
        lines,
        skipped: file_truncated,
        skip_reason: file_truncated.then(|| format!("truncated after {MAX_LINES_PER_FILE} lines")),
    }
}

fn git(root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| format!("git spawn failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(if err.is_empty() {
            format!("git {:?} failed", args)
        } else {
            err
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

fn path_is_tracked(root: &Path, rel: &str) -> bool {
    Command::new("git")
        .args(["ls-files", "--error-unmatch", "--", rel])
        .current_dir(root)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn is_probably_binary(path: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(path) else {
        return false;
    };
    use std::io::Read;
    let mut buf = [0u8; 8192];
    let n = f.read(&mut buf).unwrap_or(0);
    if n == 0 {
        return false;
    }
    buf[..n].contains(&0)
}

/// Read file if under max_bytes; `Ok(None)` if too large.
fn read_text_limited(path: &Path, max_bytes: u64) -> Result<Option<String>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > max_bytes {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.contains(&0) {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // --quiet / -q: keep `git init` / `git commit` chatter out of test output.
        assert!(Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        // identity for commit
        let _ = Command::new("git")
            .args(["config", "user.email", "t@t.com"])
            .current_dir(root)
            .status();
        let _ = Command::new("git")
            .args(["config", "user.name", "t"])
            .current_dir(root)
            .status();
        fs::write(root.join("a.txt"), "hello\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "a.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-q", "-m", "init"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        dir
    }

    #[test]
    fn unstaged_modified_and_untracked_listed_staged_only_ignored() {
        let dir = init_repo();
        let root = dir.path();
        fs::write(root.join("a.txt"), "hello\nworld\n").unwrap();
        fs::write(root.join("new.txt"), "fresh\n").unwrap();
        // Stage a different file only — should not appear as unstaged path for b if fully staged.
        fs::write(root.join("staged.txt"), "s\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "staged.txt"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());

        let paths = list_unstaged_paths(root).unwrap();
        assert!(paths.iter().any(|p| p == "a.txt"));
        assert!(paths.iter().any(|p| p == "new.txt"));
        assert!(!paths.iter().any(|p| p == "staged.txt"));

        let model = build_diff_model(root).unwrap();
        assert!(model.files.iter().any(|f| f.path == "a.txt"));
        assert!(model
            .files
            .iter()
            .any(|f| f.path == "new.txt" && f.kind == FileChangeKind::Untracked));
    }

    #[test]
    fn stage_all_adds_untracked() {
        let dir = init_repo();
        let root = dir.path();
        fs::write(root.join("u.txt"), "u\n").unwrap();
        let r = stage_all(root).unwrap();
        assert!(r.paths.iter().any(|p| p == "u.txt"));
        // after stage, unstaged list empty
        assert!(list_unstaged_paths(root).unwrap().is_empty());
    }

    #[test]
    fn fingerprint_stable() {
        let a = paths_fingerprint(&["b".into(), "a".into()]);
        let b = paths_fingerprint(&["a".into(), "b".into()]);
        assert_eq!(a, b);
    }

    /// `diff_stat`（spec gui-agent-console C15）：已跟踪走 numstat、未跟踪按行数、分类正确。
    #[test]
    fn diff_stat_counts_tracked_and_untracked() {
        let dir = init_repo();
        let root = dir.path();
        fs::write(root.join("a.txt"), "hello\nworld\nmore\n").unwrap();
        fs::write(root.join("new.txt"), "one\ntwo\n").unwrap();
        let stats = diff_stat(root).unwrap();
        let a = stats.iter().find(|s| s.path == "a.txt").unwrap();
        assert_eq!(a.kind, "M");
        assert!(a.adds >= 2, "a.txt adds={}", a.adds);
        let n = stats.iter().find(|s| s.path == "new.txt").unwrap();
        assert_eq!(n.kind, "A");
        assert_eq!(n.adds, 2);
        assert_eq!(n.dels, 0);
    }

    /// `diff_file`：单文件 hunk 视图（红绿行分类），未跟踪整文件为 Insert。
    #[test]
    fn diff_file_returns_hunks_for_single_path() {
        let dir = init_repo();
        let root = dir.path();
        fs::write(root.join("a.txt"), "hello\nchanged\n").unwrap();
        let f = diff_file(root, "a.txt").unwrap();
        assert!(f.lines.iter().any(|l| l.kind == LineKind::Insert));
        fs::write(root.join("u.txt"), "x\ny\n").unwrap();
        let u = diff_file(root, "u.txt").unwrap();
        assert_eq!(u.kind, FileChangeKind::Untracked);
        assert_eq!(u.lines.len(), 2);
    }

    /// `stage_paths`：只暂存白名单内路径；未知/越权路径被忽略。
    #[test]
    fn stage_paths_whitelists_known_paths_only() {
        let dir = init_repo();
        let root = dir.path();
        fs::write(root.join("a.txt"), "hello\nchanged\n").unwrap();
        fs::write(root.join("u.txt"), "u\n").unwrap();
        let r = stage_paths(
            root,
            &["u.txt".into(), "../etc/passwd".into(), "-rf".into()],
        )
        .unwrap();
        assert_eq!(r.paths, vec!["u.txt".to_string()]);
        let rest = list_unstaged_paths(root).unwrap();
        assert!(rest.iter().any(|p| p == "a.txt"), "a.txt 应仍未暂存");
        assert!(!rest.iter().any(|p| p == "u.txt"));
    }
}
