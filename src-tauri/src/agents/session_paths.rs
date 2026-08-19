//! Validated runtime transcript-path hints for agents whose session directory is configurable.

use super::AgentKind;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

const MAX_PATH_CHARS: usize = 8_192;
const MAX_HEADER_BYTES: u64 = 64 * 1024;
const MAX_CACHED_PATHS: usize = 1024;

type Key = (AgentKind, String);

#[derive(Default)]
struct PathCache {
    entries: HashMap<Key, PathBuf>,
    order: VecDeque<Key>,
}

fn paths() -> &'static Mutex<PathCache> {
    static PATHS: OnceLock<Mutex<PathCache>> = OnceLock::new();
    PATHS.get_or_init(|| Mutex::new(PathCache::default()))
}

/// Validate and cache a Pi v3 JSONL path. Returns the canonical path for persistence.
pub fn register_pi(session_id: &str, raw_path: &str, cwd: Option<&str>) -> Option<String> {
    if session_id.trim().is_empty()
        || raw_path.trim().is_empty()
        || raw_path.chars().count() > MAX_PATH_CHARS
    {
        return None;
    }
    let path = std::fs::canonicalize(raw_path).ok()?;
    if !path.is_file() {
        return None;
    }
    let file = std::fs::File::open(&path).ok()?;
    let mut header_line = String::new();
    BufReader::new(file)
        .take(MAX_HEADER_BYTES)
        .read_line(&mut header_line)
        .ok()?;
    let header: Value = serde_json::from_str(&header_line).ok()?;
    if header.get("type").and_then(Value::as_str) != Some("session")
        || header.get("version").and_then(Value::as_u64) != Some(3)
        || header.get("id").and_then(Value::as_str) != Some(session_id)
    {
        return None;
    }
    if let (Some(expected), Some(actual)) = (cwd, header.get("cwd").and_then(Value::as_str)) {
        if !crate::path_identity::equivalent(expected, actual) {
            return None;
        }
    }
    let key = (AgentKind::Pi, session_id.to_string());
    let mut cache = paths().lock().unwrap();
    cache.order.retain(|existing| existing != &key);
    cache.order.push_back(key.clone());
    cache.entries.insert(key, path.clone());
    while cache.entries.len() > MAX_CACHED_PATHS {
        if let Some(oldest) = cache.order.pop_front() {
            cache.entries.remove(&oldest);
        }
    }
    Some(path.to_string_lossy().to_string())
}

pub fn get(kind: AgentKind, session_id: &str) -> Option<PathBuf> {
    let key = (kind, session_id.to_string());
    let mut cache = paths().lock().unwrap();
    let path = cache.entries.get(&key).cloned()?;
    cache.order.retain(|existing| existing != &key);
    cache.order.push_back(key);
    path.is_file().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;

    #[test]
    fn validates_pi_header_identity_and_cwd() {
        let dir = tempfile::tempdir().unwrap();
        let transcript = dir.path().join("session.jsonl");
        let mut file = std::fs::File::create(&transcript).unwrap();
        writeln!(
            file,
            "{}",
            serde_json::json!({
                "type": "session",
                "version": 3,
                "id": "pi-session",
                "cwd": dir.path(),
            })
        )
        .unwrap();
        let registered = register_pi(
            "pi-session",
            &transcript.to_string_lossy(),
            Some(&dir.path().to_string_lossy()),
        )
        .unwrap();
        assert_eq!(
            Path::new(&registered),
            std::fs::canonicalize(transcript).unwrap()
        );
        assert!(register_pi("other", &registered, None).is_none());
        assert!(register_pi("pi-session", &registered, Some("/different")).is_none());
    }
}
