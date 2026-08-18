//! Daemon 生命周期支撑：二进制指纹、运行元信息（daemon.json）、跨平台单实例锁。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 可执行文件指纹：用 size + 内容哈希判定「盘上二进制内容是否变化」。
///
/// 刻意**不取 mtime**：同一份字节复制到不同位置、或重装同版本后 mtime 会变，但内容相同，
/// 应视为同一实例。若把 mtime 计入指纹，多处安装会互相误判为「二进制换了」从而反复重启
/// daemon（ping-pong）。改用内容哈希后：同内容→同指纹（与路径/mtime 无关）；内容变化
/// （dev 改码重编）→哈希变→仍会自动换新。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub size: u64,
    pub hash: u64,
}

/// 计算当前可执行文件的指纹（解析失败回退到全 0）。
///
/// 哈希基于文件内容，因此每次调用都要读盘；为避免每次 CLI 调用都重哈希几 MB 的二进制，
/// 按 (路径, mtime, size) 做持久缓存（`~/.askhuman/binhash.json`）：命中即复用已算哈希，
/// 把稳态开销降到一次。缓存仅为加速，任何缺失/损坏都会回退到重新计算。
pub fn current_fingerprint() -> Fingerprint {
    let Ok(path) = std::env::current_exe() else {
        return Fingerprint { size: 0, hash: 0 };
    };
    fingerprint_at(&path)
}

/// Compute the content fingerprint at a stable executable path.
///
/// GUI Host caches its launch path before an in-place update. On Linux, `current_exe()` may resolve
/// to a non-existent `... (deleted)` path after the running inode is replaced, so update detection
/// and re-exec must keep using the pre-update disk path instead.
pub fn fingerprint_at(path: &Path) -> Fingerprint {
    let zero = Fingerprint { size: 0, hash: 0 };
    let Ok(meta) = std::fs::metadata(path) else {
        return zero;
    };
    let size = meta.len();
    let mtime_ms = mtime_ms_of(&meta);
    if let Some(hash) = cached_hash(path, size, mtime_ms) {
        return Fingerprint { size, hash };
    }
    let hash = hash_file(path).unwrap_or(0);
    store_cached_hash(path, size, mtime_ms, hash);
    Fingerprint { size, hash }
}

/// 文件 mtime（毫秒）；解析失败回退 0。仅用作内容哈希缓存的键。
fn mtime_ms_of(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 流式读取文件并计算内容哈希。
///
/// 用标准库 `DefaultHasher`（SipHash，固定 key），跨进程/跨次运行结果一致——这点很关键，
/// 因为 client 与 daemon 必须对同一份字节得到相同哈希。逐块 `write` 与一次性 write 等价
/// （不带长度前缀），分块大小不影响结果。
fn hash_file(path: &Path) -> std::io::Result<u64> {
    use std::hash::Hasher;
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Ok(hasher.finish())
}

/// 内容哈希缓存文件 `~/.askhuman/binhash.json`（路径 → 该路径上次算过的内容哈希）。
fn hash_cache_path() -> PathBuf {
    crate::paths::config_dir().join("binhash.json")
}

/// 单条缓存：某路径在给定 (mtime, size) 下的内容哈希。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HashCacheEntry {
    mtime_ms: u64,
    size: u64,
    hash: u64,
}

type HashCache = HashMap<String, HashCacheEntry>;

fn read_hash_cache() -> HashCache {
    std::fs::read(hash_cache_path())
        .ok()
        .and_then(|d| serde_json::from_slice(&d).ok())
        .unwrap_or_default()
}

/// 缓存命中（同路径、mtime+size 未变）则返回已算哈希；否则 `None`（需重算）。
fn cached_hash(path: &Path, size: u64, mtime_ms: u64) -> Option<u64> {
    let map = read_hash_cache();
    let e = map.get(path.to_string_lossy().as_ref())?;
    (e.size == size && e.mtime_ms == mtime_ms).then_some(e.hash)
}

/// 写回缓存（best-effort）：原子落盘；不存在配置目录则跳过（不为缓存而创建目录，避免在
/// 测试 / 首次运行时污染用户目录）。无变化则不写。
fn store_cached_hash(path: &Path, size: u64, mtime_ms: u64, hash: u64) {
    let cache_path = hash_cache_path();
    let Some(dir) = cache_path.parent() else {
        return;
    };
    if !dir.exists() {
        return;
    }
    let key = path.to_string_lossy().into_owned();
    let mut map = read_hash_cache();
    if let Some(e) = map.get(&key) {
        if e.size == size && e.mtime_ms == mtime_ms && e.hash == hash {
            return;
        }
    }
    map.insert(
        key,
        HashCacheEntry {
            mtime_ms,
            size,
            hash,
        },
    );
    let Ok(data) = serde_json::to_vec(&map) else {
        return;
    };
    let tmp = cache_path.with_extension("json.tmp");
    if std::fs::write(&tmp, &data).is_ok() {
        let _ = std::fs::rename(&tmp, &cache_path);
    }
}

/// 单实例锁文件 `~/.askhuman/daemon.lock`。
pub fn lock_path() -> PathBuf {
    crate::paths::config_dir().join("daemon.lock")
}

/// 运行元信息文件 `~/.askhuman/daemon.json`。
pub fn meta_path() -> PathBuf {
    crate::paths::config_dir().join("daemon.json")
}

/// 运行日志 `~/.askhuman/daemon.log`。
pub fn log_path() -> PathBuf {
    crate::paths::config_dir().join("daemon.log")
}

/// Privacy-safe audit context for a guard decision ("suppressed" or "passed") taken before
/// any side effects.
///
/// Keep this deliberately identifier-only: prompts, answers, transcript paths, and arbitrary
/// metadata must never enter the daemon log through this interface. `thread_source` carries
/// the raw Codex thread origin label so future host-side renames are diagnosable from logs.
#[derive(Debug, Clone, Copy)]
pub struct GuardAudit<'a> {
    pub component: &'a str,
    pub action: &'static str,
    pub reason: &'a str,
    pub tool: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub thread_source: Option<&'a str>,
    pub session_id: Option<&'a str>,
    pub thread_id: Option<&'a str>,
    pub turn_id: Option<&'a str>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GuardAuditLine<'a> {
    timestamp_ms: u64,
    pid: u32,
    event: &'static str,
    component: &'a str,
    action: &'static str,
    reason: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_source: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    turn_id: Option<&'a str>,
}

fn guard_audit_line_at(audit: GuardAudit<'_>, timestamp_ms: u64, pid: u32) -> Option<String> {
    serde_json::to_string(&GuardAuditLine {
        timestamp_ms,
        pid,
        event: "askhuman_guard",
        component: audit.component,
        action: audit.action,
        reason: audit.reason,
        tool: audit.tool,
        agent: audit.agent,
        thread_source: audit.thread_source,
        session_id: audit.session_id,
        thread_id: audit.thread_id,
        turn_id: audit.turn_id,
    })
    .ok()
}

/// Append one structured guard decision to `daemon.log` (best-effort).
///
/// The write is disabled in unit-test builds so handler tests never touch the user's real log.
pub fn log_guard_audit(audit: GuardAudit<'_>) {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let Some(mut line) = guard_audit_line_at(audit, timestamp_ms, std::process::id()) else {
        return;
    };
    line.push('\n');

    #[cfg(not(test))]
    {
        use std::io::Write;

        let path = log_path();
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    #[cfg(test)]
    let _ = line;
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeEventLine<'a> {
    timestamp_ms: u64,
    pid: u32,
    event: &'static str,
    component: &'a str,
    action: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_id: Option<&'a str>,
}

/// Append one privacy-safe process/window lifecycle event to `daemon.log` (best-effort).
///
/// Only fixed action labels and an opaque request UUID are accepted. Prompt text, answers,
/// attachments, paths, channel identities, and arbitrary error strings must not use this API.
pub fn log_runtime_event(component: &str, action: &str, request_id: Option<&str>) {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    let Ok(mut line) = serde_json::to_string(&RuntimeEventLine {
        timestamp_ms,
        pid: std::process::id(),
        event: "askhuman_runtime",
        component,
        action,
        request_id,
    }) else {
        return;
    };
    line.push('\n');

    #[cfg(not(test))]
    {
        use std::io::Write;

        let path = log_path();
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

/// daemon.log 轮转阈值：超过即把现有内容挪到 `daemon.log.1`（覆盖上一代）并清空当前文件。
/// 上限约束为「两代 × 5MB」，正常运行量级下够追溯数周。
const LOG_ROTATE_LIMIT: u64 = 5 * 1024 * 1024;

/// 超限则轮转 daemon.log（copy → truncate）。daemon 启动时与周期任务里调用。
///
/// 必须「copy 到 .1 再原地 truncate」而非 rename：daemon / spawn 的 stderr fd 以 O_APPEND
/// 长期指着当前 inode，rename 后写入会跟着旧 inode 进 .1；truncate 保住 inode，
/// O_APPEND 写自动从新 EOF（0）继续。竞态无虞：写者只 append，daemon 单实例（flock）。
pub fn rotate_log_if_needed() {
    let path = log_path();
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    if meta.len() <= LOG_ROTATE_LIMIT {
        return;
    }
    let old = path.with_extension("log.1");
    if std::fs::copy(&path, &old).is_ok() {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&path) {
            let _ = f.set_len(0);
        }
    }
}

/// Daemon 运行元信息（落 daemon.json，供调试/排查）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonMeta {
    pub pid: u32,
    pub version: String,
    pub protocol_version: u32,
    pub started_at: u64,
    pub socket: String,
    pub fingerprint: Fingerprint,
}

pub fn write_meta(meta: &DaemonMeta) -> std::io::Result<()> {
    if let Some(dir) = meta_path().parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_vec_pretty(meta)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(meta_path(), data)
}

/// 持有期间代表「本进程为唯一 Daemon」。Drop 时系统文件锁自动释放。
pub type LockGuard = crate::file_lock::FileLock;

/// 尝试获取单实例锁（非阻塞）。
/// - `Ok(Some(guard))`：成功，本进程是唯一 Daemon。
/// - `Ok(None)`：已有其它 Daemon 持锁。
/// - `Err`：其它 IO 错误。
pub fn acquire_lock() -> std::io::Result<Option<LockGuard>> {
    acquire_lock_at(&lock_path())
}

/// 在指定路径上尝试获取单实例文件锁（非阻塞）。供 daemon（`daemon.lock`）与
/// GUI 宿主（`gui-host.lock`）共用。返回值语义同 `acquire_lock`。
pub fn acquire_lock_at(path: &Path) -> std::io::Result<Option<LockGuard>> {
    crate::file_lock::FileLock::try_exclusive(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_reflects_current_exe() {
        // 测试可执行文件存在 → size 非 0，且两次取值一致（盘上未变）。
        let a = current_fingerprint();
        let b = current_fingerprint();
        assert!(a.size > 0);
        assert_eq!(a, b);
    }

    #[test]
    fn hash_is_content_identity_not_path_or_mtime() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("ah-fp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bytes = b"AskHuman binary content sample";

        // 同内容、不同路径 → 同哈希（与路径无关）。
        let p1 = dir.join("a/AskHuman");
        let p2 = dir.join("b/AskHuman");
        std::fs::create_dir_all(p1.parent().unwrap()).unwrap();
        std::fs::create_dir_all(p2.parent().unwrap()).unwrap();
        std::fs::File::create(&p1)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        std::fs::File::create(&p2)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        assert_eq!(hash_file(&p1).unwrap(), hash_file(&p2).unwrap());

        // 内容不同 → 哈希不同。
        let p3 = dir.join("c/AskHuman");
        std::fs::create_dir_all(p3.parent().unwrap()).unwrap();
        std::fs::File::create(&p3)
            .unwrap()
            .write_all(b"different")
            .unwrap();
        assert_ne!(hash_file(&p1).unwrap(), hash_file(&p3).unwrap());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fingerprint_at_tracks_atomic_replacement() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("AskHuman");
        std::fs::File::create(&path)
            .unwrap()
            .write_all(b"old binary")
            .unwrap();
        let old = fingerprint_at(&path);

        let staged = dir.path().join(".AskHuman.new");
        std::fs::File::create(&staged)
            .unwrap()
            .write_all(b"new binary with different bytes")
            .unwrap();
        std::fs::rename(staged, &path).unwrap();

        let new = fingerprint_at(&path);
        assert_ne!(old, new);
        assert_eq!(new.size, 31);
    }

    #[test]
    fn meta_round_trip() {
        let meta = DaemonMeta {
            pid: 1,
            version: "9.9.9".into(),
            protocol_version: 1,
            started_at: 100,
            socket: "/tmp/x.sock".into(),
            fingerprint: Fingerprint { size: 6, hash: 5 },
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: DaemonMeta = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pid, 1);
        assert_eq!(back.version, "9.9.9");
        assert_eq!(back.fingerprint.size, 6);
        assert_eq!(back.fingerprint.hash, 5);
    }

    #[test]
    fn guard_audit_is_structured_and_omits_missing_or_sensitive_fields() {
        let line = guard_audit_line_at(
            GuardAudit {
                component: "mcp_tool",
                action: "suppressed",
                reason: "codex_blocked_thread_source",
                tool: Some("whats_next"),
                agent: Some("codex"),
                thread_source: Some("ambient_suggestions"),
                session_id: Some("session-1"),
                thread_id: Some("thread-1"),
                turn_id: None,
            },
            123,
            456,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["timestampMs"], 123);
        assert_eq!(value["pid"], 456);
        assert_eq!(value["event"], "askhuman_guard");
        assert_eq!(value["component"], "mcp_tool");
        assert_eq!(value["action"], "suppressed");
        assert_eq!(value["reason"], "codex_blocked_thread_source");
        assert_eq!(value["tool"], "whats_next");
        assert_eq!(value["agent"], "codex");
        assert_eq!(value["threadSource"], "ambient_suggestions");
        assert_eq!(value["sessionId"], "session-1");
        assert_eq!(value["threadId"], "thread-1");
        assert!(value.get("turnId").is_none());
        assert!(!line.contains("prompt"));
        assert!(!line.contains("transcript"));
    }

    #[test]
    fn guard_audit_pass_action_omits_thread_source_when_absent() {
        let line = guard_audit_line_at(
            GuardAudit {
                component: "mcp_tool",
                action: "passed",
                reason: "codex_thread_source_missing",
                tool: Some("ask"),
                agent: Some("codex"),
                thread_source: None,
                session_id: None,
                thread_id: Some("thread-2"),
                turn_id: None,
            },
            123,
            456,
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["action"], "passed");
        assert_eq!(value["reason"], "codex_thread_source_missing");
        assert!(value.get("threadSource").is_none());
        assert_eq!(value["threadId"], "thread-2");
    }
}
