//! Self-update state in `~/.askhuman/update.json`: latest version, notes, check time, dismissed
//! versions, and the pending-restart flag. Writes are best-effort and atomic; Unix writers are also
//! serialized across processes so the daemon, GUI Host, and Settings cannot overwrite fields.

use crate::paths;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UpdateState {
    /// Latest stable release observed by any process, normalized without a `v` prefix.
    pub latest_version: String,
    /// Release notes for `latest_version`.
    pub release_notes: String,
    /// Time of the most recent successful remote check (Unix seconds).
    pub checked_at: u64,
    /// Versions the user dismissed from proactive update prompts.
    pub dismissed_versions: Vec<String>,
    /// A new binary is on disk and waiting for the daemon/GUI Host to switch over.
    pub pending: bool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Read the current state. A missing or malformed file degrades to defaults.
pub fn load() -> UpdateState {
    load_at(&paths::update_state_file())
}

fn load_at(path: &Path) -> UpdateState {
    std::fs::read(path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

/// Atomic best-effort write. Unique temp names also avoid collisions on platforms where advisory
/// locking is unavailable.
fn store_at(path: &Path, state: &UpdateState) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(data) = serde_json::to_vec_pretty(state) else {
        return;
    };
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    if std::fs::write(&tmp, &data).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

fn mutate_at(path: &Path, lock: &Path, mutate: impl FnOnce(&mut UpdateState)) -> UpdateState {
    let _guard = lock_at(lock);
    let mut state = load_at(path);
    mutate(&mut state);
    store_at(path, &state);
    state
}

/// Record a successful check. The cached latest version is monotonic so a briefly stale remote
/// response cannot downgrade a version another process has already observed. Manual checks clear
/// dismissed versions in the same transaction, after the network check succeeds.
pub fn record_check(
    latest_version: &str,
    release_notes: &str,
    clear_dismissed: bool,
) -> UpdateState {
    record_check_at(
        &paths::update_state_file(),
        &paths::update_state_lock(),
        latest_version,
        release_notes,
        clear_dismissed,
    )
}

fn record_check_at(
    path: &Path,
    lock: &Path,
    latest_version: &str,
    release_notes: &str,
    clear_dismissed: bool,
) -> UpdateState {
    mutate_at(path, lock, |state| {
        if state.latest_version.is_empty()
            || super::compare_versions(latest_version, &state.latest_version) >= 0
        {
            state.latest_version = latest_version.to_string();
            state.release_notes = release_notes.to_string();
        }
        if clear_dismissed {
            state.dismissed_versions.clear();
        }
        state.checked_at = now_secs();
    })
}

/// Whether a version is currently dismissed.
pub fn is_dismissed(version: &str) -> bool {
    load().dismissed_versions.iter().any(|item| item == version)
}

/// Dismiss one version from proactive prompts.
pub fn dismiss(version: &str) {
    mutate_at(
        &paths::update_state_file(),
        &paths::update_state_lock(),
        |state| {
            if !state.dismissed_versions.iter().any(|item| item == version) {
                state.dismissed_versions.push(version.to_string());
            }
        },
    );
}

/// Clear all dismissed versions.
pub fn clear_dismissed() {
    mutate_at(
        &paths::update_state_file(),
        &paths::update_state_lock(),
        |state| state.dismissed_versions.clear(),
    );
}

/// Set or clear the pending-restart flag.
pub fn set_pending(pending: bool) {
    mutate_at(
        &paths::update_state_file(),
        &paths::update_state_lock(),
        |state| state.pending = pending,
    );
}

// ===== Cross-process write lock =====

fn lock_at(path: &Path) -> Option<crate::file_lock::FileLock> {
    crate::file_lock::FileLock::exclusive(path).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_paths() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("update.json");
        let lock = dir.path().join("update.lock");
        (dir, state, lock)
    }

    #[test]
    fn record_check_never_downgrades_and_preserves_other_fields() {
        let (_dir, path, lock) = test_paths();
        store_at(
            &path,
            &UpdateState {
                latest_version: "0.9.4".to_string(),
                release_notes: "new".to_string(),
                dismissed_versions: vec!["0.9.3".to_string()],
                pending: true,
                ..Default::default()
            },
        );

        let stored = record_check_at(&path, &lock, "0.9.3", "old", false);
        assert_eq!(stored.latest_version, "0.9.4");
        assert_eq!(stored.release_notes, "new");
        assert_eq!(stored.dismissed_versions, vec!["0.9.3"]);
        assert!(stored.pending);
        assert!(stored.checked_at > 0);
        assert_eq!(load_at(&path), stored);
    }

    #[test]
    fn manual_record_clears_dismissed_in_same_transaction() {
        let (_dir, path, lock) = test_paths();
        store_at(
            &path,
            &UpdateState {
                latest_version: "0.9.3".to_string(),
                dismissed_versions: vec!["0.9.3".to_string()],
                ..Default::default()
            },
        );

        let stored = record_check_at(&path, &lock, "0.9.4", "notes", true);
        assert_eq!(stored.latest_version, "0.9.4");
        assert!(stored.dismissed_versions.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_mutations_preserve_independent_fields() {
        let (_dir, path, lock) = test_paths();
        let handles = [0, 1, 2]
            .into_iter()
            .map(|operation| {
                let path = path.clone();
                let lock = lock.clone();
                std::thread::spawn(move || match operation {
                    0 => {
                        record_check_at(&path, &lock, "0.9.4", "notes", false);
                    }
                    1 => {
                        mutate_at(&path, &lock, |state| state.pending = true);
                    }
                    _ => {
                        mutate_at(&path, &lock, |state| {
                            state.dismissed_versions.push("0.9.3".to_string())
                        });
                    }
                })
            })
            .collect::<Vec<_>>();
        for handle in handles {
            handle.join().unwrap();
        }

        let stored = load_at(&path);
        assert_eq!(stored.latest_version, "0.9.4");
        assert!(stored.pending);
        assert_eq!(stored.dismissed_versions, vec!["0.9.3"]);
    }
}
