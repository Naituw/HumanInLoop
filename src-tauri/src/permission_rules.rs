//! Codex permission memory rule store (spec `docs/specs/codex-permission-remember.md`).
//!
//! Shadow rules recording the user's "allow in this conversation" / D41 cross-session
//! decisions. `~/.askhuman/state/codex_rules.json` is the single source of truth: every
//! mutation takes an exclusive advisory lock (`codex_rules.lock`) around the
//! read-modify-write, the file is written atomically (tmp + rename) with 0600 permissions,
//! and rules expire after 30 days without an auto-allow hit (D15). Capacity is capped
//! (session 500 / store-wide 10000, §6.2); when full, new rules are rejected so the caller
//! degrades to allow-once per D25 instead of silently evicting existing grants.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Rolling retention window: a rule is dropped after this long without an auto-allow hit (D15).
pub const RULE_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;
/// Hard cap of rules under one Codex session id (§6.2 storage baseline).
pub const SESSION_RULE_CAP: usize = 500;
/// Hard cap of rules across the whole store, all sessions plus the global namespace (§6.2).
pub const STORE_RULE_CAP: usize = 10_000;

/// One shadow rule key. Matching semantics per kind are documented on [`query_hits`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RuleKey {
    /// Exact lexically-normalized absolute file path (D9/D46).
    FileExact { path: String },
    /// Every file-edit path within this project root (D10).
    FileProject { root: String },
    /// Any verifiable native file edit in the session (D11).
    FileDisk,
    /// MCP tool: the full hook `tool_name` string, e.g. `mcp__server__tool` (D40/D41).
    McpTool { tool: String },
    /// Network host grant: lowercase host + protocol + port (D39).
    NetworkHost {
        host: String,
        protocol: String,
        port: u16,
    },
    /// One exact shell command segment: full argv token match (D38).
    ShellExact { argv: Vec<String> },
    /// Shell command prefix (D38, model-provided prefix_rule only).
    ShellPrefix { prefix: Vec<String> },
    /// Relaxed mode for this session (D52): auto-allow splittable shell commands that
    /// hit no dangerous list; only checked via [`check_relaxed_auto_allow`], never via
    /// [`query_hits`].
    ShellRelaxed,
    /// YOLO mode for this session (D53): auto-allow every Codex permission request,
    /// dangerous ones included. Only checked via [`check_yolo_auto_allow`], never via
    /// [`query_hits`]; turned off via [`disable_yolo`] (panel button / IM `/yolo`).
    Yolo,
}

impl RuleKey {
    /// Human-readable key text for the management panel (D48: raw, not redacted).
    pub fn display(&self) -> String {
        match self {
            Self::FileExact { path } => path.clone(),
            Self::FileProject { root } => root.clone(),
            Self::FileDisk => String::new(),
            Self::McpTool { tool } => tool.clone(),
            Self::NetworkHost {
                host,
                protocol,
                port,
            } => format!("{protocol}://{host}:{port}"),
            Self::ShellExact { argv } => argv.join(" "),
            Self::ShellPrefix { prefix } => format!("{} …", prefix.join(" ")),
            Self::ShellRelaxed => String::new(),
            Self::Yolo => String::new(),
        }
    }

    /// Basic structural sanity used by daemon-side validation of hook-provided saves.
    pub fn is_valid(&self) -> bool {
        const MAX_TEXT: usize = 8_192;
        let text_ok = |value: &str| !value.is_empty() && value.len() <= MAX_TEXT;
        match self {
            Self::FileExact { path } => text_ok(path) && path_is_absolute_text(path),
            Self::FileProject { root } => text_ok(root) && path_is_absolute_text(root),
            Self::FileDisk => true,
            Self::McpTool { tool } => text_ok(tool) && tool.starts_with("mcp__"),
            Self::NetworkHost { host, protocol, .. } => {
                text_ok(host)
                    && matches!(
                        protocol.as_str(),
                        "http" | "https" | "socks5-tcp" | "socks5-udp"
                    )
            }
            Self::ShellExact { argv } => {
                !argv.is_empty() && argv.iter().all(|token| token.len() <= MAX_TEXT)
            }
            Self::ShellPrefix { prefix } => {
                !prefix.is_empty() && prefix.iter().all(|token| text_ok(token))
            }
            Self::ShellRelaxed => true,
            Self::Yolo => true,
        }
    }
}

/// What a permission request needs covered for a silent auto-allow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum MemoryQuery {
    /// All old/new paths of a verifiable native file edit (lexically absolute, D46).
    FileEdit { paths: Vec<String> },
    /// Full hook tool_name of an MCP call (D40). Checked against session + global rules.
    McpTool { tool: String },
    /// Network host approval (D39, already通过判真).
    NetworkHost {
        host: String,
        protocol: String,
        port: u16,
    },
    /// Every split segment of a shell script must be covered (D38).
    ShellCommands {
        commands: Vec<Vec<String>>,
        /// Hook-side verdict for relaxed mode (D52): splittable, no segment on the
        /// extended dangerous list, no native prompt/forbidden hit. Only such requests
        /// may be auto-allowed by relaxed mode; rule-based matching ignores this flag.
        #[serde(default)]
        relaxed_eligible: bool,
    },
}

/// Rule namespace a save targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuleNamespace {
    /// Scoped to one Codex conversation tree (session_id, D4/D5).
    Session,
    /// Cross-session fallback namespace (D41; plugin / codex_apps MCP only).
    Global,
}

/// One stored rule with its rolling-retention bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredRule {
    pub key: RuleKey,
    #[serde(default)]
    pub created_at_ms: u64,
    #[serde(default)]
    pub last_used_at_ms: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct RuleFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    sessions: HashMap<String, Vec<StoredRule>>,
    #[serde(default)]
    global: Vec<StoredRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SaveError {
    /// Session or store-wide cap reached; caller degrades per D25.
    CapacityExceeded,
    /// Persisting the store file failed.
    Io,
    /// Key failed structural validation.
    InvalidRule,
}

/// Per-session summary for the management panel (§6.3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRuleSummary {
    pub session_id: String,
    pub rule_count: usize,
    pub file_exact_count: usize,
    pub project_roots: Vec<String>,
    pub full_disk: bool,
    pub shell_count: usize,
    pub network_count: usize,
    pub mcp_count: usize,
    /// YOLO mode active for this session (D53).
    #[serde(default)]
    pub yolo: bool,
    pub last_used_at_ms: u64,
}

fn rules_file() -> PathBuf {
    crate::paths::state_dir().join("codex_rules.json")
}

fn rules_lock() -> PathBuf {
    crate::paths::state_dir().join("codex_rules.lock")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn load_at(path: &Path) -> RuleFile {
    let Ok(text) = std::fs::read_to_string(path) else {
        return RuleFile::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn store_at(path: &Path, mut data: RuleFile) -> bool {
    data.version = 1;
    data.sessions.retain(|_, rules| !rules.is_empty());
    let Ok(json) = serde_json::to_string(&data) else {
        return false;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    if std::fs::write(&tmp, json.as_bytes()).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).is_ok()
}

/// Drop rules whose last hit is older than the retention window (D15).
fn prune_expired(data: &mut RuleFile, now: u64) {
    let keep = |rule: &StoredRule| {
        let anchor = rule.last_used_at_ms.max(rule.created_at_ms);
        now.saturating_sub(anchor) <= RULE_TTL_MS
    };
    for rules in data.sessions.values_mut() {
        rules.retain(keep);
    }
    data.global.retain(keep);
    data.sessions.retain(|_, rules| !rules.is_empty());
}

fn total_rules(data: &RuleFile) -> usize {
    data.sessions.values().map(Vec::len).sum::<usize>() + data.global.len()
}

// ===== Cross-process write lock (same pattern as todos.rs) =====

fn lock_at(path: &Path) -> Option<crate::file_lock::FileLock> {
    crate::file_lock::FileLock::exclusive(path).ok()
}

// ===== Matching =====

pub(crate) fn path_is_absolute_text(value: &str) -> bool {
    value.starts_with('/') || Path::new(value).is_absolute()
}

fn comparable_path(value: &str) -> String {
    crate::path_identity::key(value)
}

/// Lexical prefix check on normalized absolute paths (component boundary aware).
pub(crate) fn path_within_text(path: &str, root: &str) -> bool {
    let path = comparable_path(path);
    let root = comparable_path(root);
    if path == root {
        return true;
    }
    let root_trimmed = root.trim_end_matches('/');
    if root_trimmed.is_empty() {
        // Root "/" contains every absolute path.
        return path.starts_with('/');
    }
    path.strip_prefix(root_trimmed)
        .is_some_and(|rest| rest.starts_with('/'))
}

fn path_within(path: &str, root: &str) -> bool {
    path_within_text(path, root)
}

/// D46: on an aggregated-scope hit, refuse auto-allow when any existing component of the
/// target path between the project root and the file is a symlink (project escape guard).
/// Purely lexically-matched exact rules skip this check by design.
fn symlink_free_within(path: &str, root: &str) -> bool {
    let root_path = Path::new(root);
    let mut current = root_path.to_path_buf();
    let Ok(rest) = Path::new(path).strip_prefix(root_path) else {
        return false;
    };
    for component in rest.components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => return false,
            _ => {}
        }
    }
    true
}

/// Indices of session rules that satisfy the query, or `None` when not covered.
/// Global-namespace rules participate only for MCP tools (D41).
fn query_hits(
    session_rules: &[StoredRule],
    global_rules: &[StoredRule],
    query: &MemoryQuery,
) -> Option<(Vec<usize>, Vec<usize>)> {
    match query {
        MemoryQuery::FileEdit { paths } => {
            if paths.is_empty() {
                return None;
            }
            // Full-disk rule covers everything verifiable.
            if let Some(index) = session_rules
                .iter()
                .position(|rule| matches!(rule.key, RuleKey::FileDisk))
            {
                return Some((vec![index], Vec::new()));
            }
            let mut hits: Vec<usize> = Vec::new();
            for path in paths {
                let found = session_rules.iter().position(|rule| match &rule.key {
                    RuleKey::FileExact { path: rule_path } => rule_path == path,
                    RuleKey::FileProject { root } => {
                        path_within(path, root) && symlink_free_within(path, root)
                    }
                    _ => false,
                })?;
                if !hits.contains(&found) {
                    hits.push(found);
                }
            }
            Some((hits, Vec::new()))
        }
        MemoryQuery::McpTool { tool } => {
            let matches_tool = |rule: &StoredRule| matches!(&rule.key, RuleKey::McpTool { tool: rule_tool } if rule_tool == tool);
            if let Some(index) = session_rules.iter().position(matches_tool) {
                return Some((vec![index], Vec::new()));
            }
            global_rules
                .iter()
                .position(matches_tool)
                .map(|index| (Vec::new(), vec![index]))
        }
        MemoryQuery::NetworkHost {
            host,
            protocol,
            port,
        } => session_rules
            .iter()
            .position(|rule| {
                matches!(&rule.key, RuleKey::NetworkHost {
                    host: rule_host,
                    protocol: rule_protocol,
                    port: rule_port,
                } if rule_host == host && rule_protocol == protocol && rule_port == port)
            })
            .map(|index| (vec![index], Vec::new())),
        MemoryQuery::ShellCommands { commands, .. } => {
            if commands.is_empty() {
                return None;
            }
            let mut hits: Vec<usize> = Vec::new();
            for argv in commands {
                let found = session_rules.iter().position(|rule| match &rule.key {
                    RuleKey::ShellExact { argv: rule_argv } => rule_argv == argv,
                    RuleKey::ShellPrefix { prefix } => {
                        argv.len() >= prefix.len() && argv[..prefix.len()] == prefix[..]
                    }
                    _ => false,
                })?;
                if !hits.contains(&found) {
                    hits.push(found);
                }
            }
            Some((hits, Vec::new()))
        }
    }
}

// ===== Public API (default paths) =====

/// Check whether stored rules cover `query`; refreshes `last_used_at` on hit (D15).
pub fn check_auto_allow(session_id: &str, query: &MemoryQuery) -> bool {
    check_auto_allow_at(&rules_file(), &rules_lock(), session_id, query)
}

/// Whether relaxed mode (D52) is enabled for this session via a stored [`RuleKey::ShellRelaxed`]
/// rule; refreshes its rolling retention on hit. Eligibility of the request itself is
/// the caller's concern (`MemoryQuery::ShellCommands::relaxed_eligible`).
pub fn check_relaxed_auto_allow(session_id: &str) -> bool {
    check_relaxed_auto_allow_at(&rules_file(), &rules_lock(), session_id)
}

/// Whether YOLO mode (D53) is enabled for this session via a stored [`RuleKey::Yolo`] rule;
/// refreshes its rolling retention on hit.
pub fn check_yolo_auto_allow(session_id: &str) -> bool {
    check_session_flag_at(&rules_file(), &rules_lock(), session_id, &RuleKey::Yolo)
}

/// Turn YOLO mode off for one session (D53): remove its [`RuleKey::Yolo`] rule only,
/// leaving every other grant intact. Returns whether a rule was removed.
pub fn disable_yolo(session_id: &str) -> bool {
    disable_yolo_at(&rules_file(), &rules_lock(), session_id)
}

/// Session ids with YOLO mode currently on (D53), most recently used first.
/// Feeds the IM `/yolo` picker.
pub fn yolo_session_ids() -> Vec<String> {
    yolo_session_ids_at(&rules_file())
}

/// Persist rules; dedup against existing keys. Errors mean the caller must degrade per D25.
pub fn save_rules(
    session_id: &str,
    namespace: RuleNamespace,
    keys: &[RuleKey],
) -> Result<(), SaveError> {
    save_rules_at(&rules_file(), &rules_lock(), session_id, namespace, keys)
}

/// Management panel: per-session summaries, most recently used first (§6.3).
pub fn session_summaries() -> Vec<SessionRuleSummary> {
    session_summaries_at(&rules_file())
}

/// Management panel: full rule list of one session.
pub fn session_rules(session_id: &str) -> Vec<StoredRule> {
    load_at(&rules_file())
        .sessions
        .get(session_id)
        .cloned()
        .unwrap_or_default()
}

/// Management panel: cross-session (D41) rules.
pub fn global_rules() -> Vec<StoredRule> {
    load_at(&rules_file()).global
}

/// Delete every rule of one session (panel "reset this conversation"). Returns removed count.
pub fn reset_session(session_id: &str) -> usize {
    reset_session_at(&rules_file(), &rules_lock(), session_id)
}

/// Delete every cross-session rule (panel "reset cross-session grants"). Returns removed count.
pub fn reset_global() -> usize {
    reset_global_at(&rules_file(), &rules_lock())
}

// ===== Path-parameterized implementations =====

pub fn check_auto_allow_at(
    path: &Path,
    lock: &Path,
    session_id: &str,
    query: &MemoryQuery,
) -> bool {
    if session_id.trim().is_empty() {
        return false;
    }
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let now = now_ms();
    prune_expired(&mut data, now);
    let empty = Vec::new();
    let session_rules = data.sessions.get(session_id).unwrap_or(&empty);
    let Some((session_hits, global_hits)) = query_hits(session_rules, &data.global, query) else {
        // Persist pruning opportunistically even on miss (best-effort).
        store_at(path, data);
        return false;
    };
    if let Some(rules) = data.sessions.get_mut(session_id) {
        for index in session_hits {
            if let Some(rule) = rules.get_mut(index) {
                rule.last_used_at_ms = now;
            }
        }
    }
    for index in global_hits {
        if let Some(rule) = data.global.get_mut(index) {
            rule.last_used_at_ms = now;
        }
    }
    store_at(path, data);
    true
}

pub fn check_relaxed_auto_allow_at(path: &Path, lock: &Path, session_id: &str) -> bool {
    check_session_flag_at(path, lock, session_id, &RuleKey::ShellRelaxed)
}

/// Shared checker for keyless session-mode rules (relaxed D52 / yolo D53): present ⇒ hit,
/// and a hit refreshes the rule's rolling retention.
pub fn check_session_flag_at(path: &Path, lock: &Path, session_id: &str, key: &RuleKey) -> bool {
    if session_id.trim().is_empty() {
        return false;
    }
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let now = now_ms();
    prune_expired(&mut data, now);
    let hit = data
        .sessions
        .get_mut(session_id)
        .and_then(|rules| rules.iter_mut().find(|rule| rule.key == *key));
    let found = match hit {
        Some(rule) => {
            rule.last_used_at_ms = now;
            true
        }
        None => false,
    };
    store_at(path, data);
    found
}

pub fn disable_yolo_at(path: &Path, lock: &Path, session_id: &str) -> bool {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let Some(rules) = data.sessions.get_mut(session_id) else {
        return false;
    };
    let before = rules.len();
    rules.retain(|rule| !matches!(rule.key, RuleKey::Yolo));
    let removed = rules.len() < before;
    if removed {
        store_at(path, data);
    }
    removed
}

pub fn yolo_session_ids_at(path: &Path) -> Vec<String> {
    let mut data = load_at(path);
    prune_expired(&mut data, now_ms());
    let mut sessions: Vec<(String, u64)> = data
        .sessions
        .iter()
        .filter_map(|(session_id, rules)| {
            rules
                .iter()
                .find(|rule| matches!(rule.key, RuleKey::Yolo))
                .map(|rule| {
                    (
                        session_id.clone(),
                        rule.last_used_at_ms.max(rule.created_at_ms),
                    )
                })
        })
        .collect();
    sessions.sort_by_key(|(_, last_used)| std::cmp::Reverse(*last_used));
    sessions
        .into_iter()
        .map(|(session_id, _)| session_id)
        .collect()
}

pub fn save_rules_at(
    path: &Path,
    lock: &Path,
    session_id: &str,
    namespace: RuleNamespace,
    keys: &[RuleKey],
) -> Result<(), SaveError> {
    if keys.is_empty() || keys.iter().any(|key| !key.is_valid()) {
        return Err(SaveError::InvalidRule);
    }
    if namespace == RuleNamespace::Session && session_id.trim().is_empty() {
        return Err(SaveError::InvalidRule);
    }
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let now = now_ms();
    prune_expired(&mut data, now);

    let existing_total = total_rules(&data);
    let target_len = match namespace {
        RuleNamespace::Session => data
            .sessions
            .get(session_id)
            .map(Vec::len)
            .unwrap_or_default(),
        RuleNamespace::Global => data.global.len(),
    };
    let target = match namespace {
        RuleNamespace::Session => data.sessions.entry(session_id.to_string()).or_default(),
        RuleNamespace::Global => &mut data.global,
    };
    let mut new_keys: Vec<&RuleKey> = Vec::new();
    for key in keys {
        if target.iter().any(|rule| rule.key == *key) || new_keys.contains(&key) {
            continue;
        }
        new_keys.push(key);
    }
    if new_keys.is_empty() {
        // Everything already stored: refresh nothing, report success.
        return Ok(());
    }
    if namespace == RuleNamespace::Session && target_len + new_keys.len() > SESSION_RULE_CAP {
        return Err(SaveError::CapacityExceeded);
    }
    if existing_total + new_keys.len() > STORE_RULE_CAP {
        return Err(SaveError::CapacityExceeded);
    }
    for key in new_keys {
        target.push(StoredRule {
            key: key.clone(),
            created_at_ms: now,
            last_used_at_ms: now,
        });
    }
    if store_at(path, data) {
        Ok(())
    } else {
        Err(SaveError::Io)
    }
}

pub fn session_summaries_at(path: &Path) -> Vec<SessionRuleSummary> {
    let data = load_at(path);
    let mut summaries: Vec<SessionRuleSummary> = data
        .sessions
        .iter()
        .map(|(session_id, rules)| {
            let mut summary = SessionRuleSummary {
                session_id: session_id.clone(),
                rule_count: rules.len(),
                file_exact_count: 0,
                project_roots: Vec::new(),
                full_disk: false,
                shell_count: 0,
                network_count: 0,
                mcp_count: 0,
                yolo: false,
                last_used_at_ms: 0,
            };
            for rule in rules {
                summary.last_used_at_ms = summary
                    .last_used_at_ms
                    .max(rule.last_used_at_ms.max(rule.created_at_ms));
                match &rule.key {
                    RuleKey::FileExact { .. } => summary.file_exact_count += 1,
                    RuleKey::FileProject { root } => {
                        if !summary.project_roots.contains(root) {
                            summary.project_roots.push(root.clone());
                        }
                    }
                    RuleKey::FileDisk => summary.full_disk = true,
                    RuleKey::McpTool { .. } => summary.mcp_count += 1,
                    RuleKey::NetworkHost { .. } => summary.network_count += 1,
                    RuleKey::ShellExact { .. }
                    | RuleKey::ShellPrefix { .. }
                    | RuleKey::ShellRelaxed => summary.shell_count += 1,
                    RuleKey::Yolo => summary.yolo = true,
                }
            }
            summary
        })
        .collect();
    summaries.sort_by_key(|summary| std::cmp::Reverse(summary.last_used_at_ms));
    summaries
}

pub fn reset_session_at(path: &Path, lock: &Path, session_id: &str) -> usize {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let removed = data
        .sessions
        .remove(session_id)
        .map(|rules| rules.len())
        .unwrap_or(0);
    if removed > 0 {
        store_at(path, data);
    }
    removed
}

pub fn reset_global_at(path: &Path, lock: &Path) -> usize {
    let _guard = lock_at(lock);
    let mut data = load_at(path);
    let removed = data.global.len();
    if removed > 0 {
        data.global.clear();
        store_at(path, data);
    }
    removed
}

// ===== Path normalization (D46) =====

/// Lexically normalize a patch path against the hook `cwd`, matching Codex `AbsolutePathBuf`
/// semantics: `~` expansion, absolute join, `.`/`..` folded without touching the filesystem
/// (no canonicalize / symlink resolution), byte-exact case-sensitive result.
pub fn normalize_path(raw: &str, cwd: &str) -> Option<String> {
    if raw.is_empty() || raw.contains('\0') || raw.chars().count() > 8_192 {
        return None;
    }
    let expanded: String = if raw == "~" {
        dirs_home()?
    } else if let Some(rest) = raw.strip_prefix("~/") {
        Path::new(&dirs_home()?)
            .join(rest)
            .to_string_lossy()
            .to_string()
    } else {
        raw.to_string()
    };

    // Keep POSIX-shaped protocol fixtures valid on every host. Native Windows paths use
    // the component-aware branch below.
    if expanded.starts_with('/') || (cwd.starts_with('/') && !Path::new(&expanded).is_absolute()) {
        let joined = if expanded.starts_with('/') {
            expanded
        } else {
            format!("{}/{expanded}", cwd.trim_end_matches('/'))
        };
        let mut parts: Vec<&str> = Vec::new();
        for component in joined.split('/') {
            match component {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                other => parts.push(other),
            }
        }
        let mut result = String::from("/");
        result.push_str(&parts.join("/"));
        return Some(result);
    }

    let expanded = PathBuf::from(expanded);
    let joined = if expanded.is_absolute() {
        expanded
    } else {
        let cwd = PathBuf::from(cwd);
        if !cwd.is_absolute() {
            return None;
        }
        cwd.join(expanded)
    };
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(
                    result.components().next_back(),
                    Some(std::path::Component::Normal(_))
                ) {
                    result.pop();
                }
            }
            other => result.push(other.as_os_str()),
        }
    }
    result
        .is_absolute()
        .then(|| result.to_string_lossy().to_string())
}

fn dirs_home() -> Option<String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .filter(|value| value.is_absolute())
        .map(|value| value.to_string_lossy().to_string())
}

// ===== Wire payload: hook -> daemon =====

/// Memory metadata carried on `ConfirmTask` (hook -> daemon). The daemon owns matching and
/// persistence (D26); the hook only describes what may be auto-allowed and what each
/// remember action id would save.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionMemory {
    /// Query for a silent auto-allow before any surface is engaged. `None` disables it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<MemoryQuery>,
    /// Save operations keyed by choice action id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub saves: Vec<MemorySave>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySave {
    pub action_id: String,
    pub namespace: RuleNamespace,
    /// Shadow rules to store. With a native write these are the session bridge rules (D40).
    pub rules: Vec<RuleKey>,
    /// Optional native Codex config write performed before the shadow rules (D7/D40).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native: Option<NativeWrite>,
}

/// A write into native Codex configuration that a remember action performs. The daemon is
/// the only writer and re-verifies the target before touching it (fail closed → D25).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum NativeWrite {
    /// `mcp_servers.<server>.tools.<tool>.approval_mode = "approve"` in `config_path`,
    /// format-preserving (D40). The server table must already exist in that file.
    McpApprovalMode {
        config_path: String,
        server: String,
        tool: String,
    },
    /// Append `network_rule(host=..., protocol=..., decision="allow")` to the Codex
    /// `rules/default.rules` file, replicating `blocking_append_network_rule` (D39/§9.3).
    NetworkRule {
        rules_path: String,
        host: String,
        protocol: String,
    },
    /// Append `prefix_rule(pattern=[...], decision="allow")` to `rules/default.rules`,
    /// replicating `blocking_append_allow_prefix_rule` (D38/§9.3). Verified against the
    /// installed Codex before writing (D45).
    PrefixRule {
        rules_path: String,
        prefix: Vec<String>,
    },
}

/// Protocols valid both for D39 truth-checking and for native `network_rule` lines.
pub const NETWORK_PROTOCOLS: [&str; 4] = ["http", "https", "socks5-tcp", "socks5-udp"];

/// Host constraints mirroring Codex `normalize_network_rule_host` on an already
/// port-free host: lowercase, no scheme/path/query/wildcard/whitespace.
pub fn network_host_is_valid(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 512
        && !host.contains("://")
        && !host.contains('/')
        && !host.contains('?')
        && !host.contains('#')
        && !host.contains('*')
        && !host.chars().any(char::is_whitespace)
        && !host.chars().any(|c| c.is_ascii_uppercase())
        && !host.ends_with('.')
}

impl NativeWrite {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::McpApprovalMode {
                config_path,
                server,
                tool,
            } => {
                path_is_absolute_text(config_path)
                    && comparable_path(config_path).ends_with("/config.toml")
                    && config_path.len() <= 4_096
                    && !server.is_empty()
                    && server.len() <= 512
                    && !tool.is_empty()
                    && tool.len() <= 512
            }
            Self::NetworkRule {
                rules_path,
                host,
                protocol,
            } => {
                path_is_absolute_text(rules_path)
                    && comparable_path(rules_path).ends_with("/rules/default.rules")
                    && rules_path.len() <= 4_096
                    && network_host_is_valid(host)
                    && NETWORK_PROTOCOLS.contains(&protocol.as_str())
            }
            Self::PrefixRule { rules_path, prefix } => {
                path_is_absolute_text(rules_path)
                    && comparable_path(rules_path).ends_with("/rules/default.rules")
                    && rules_path.len() <= 4_096
                    && !prefix.is_empty()
                    && prefix.len() <= 64
                    && prefix
                        .iter()
                        .all(|token| !token.is_empty() && token.len() <= 4_096)
                    && !crate::shell_safety::is_banned_prefix(prefix)
            }
        }
    }
}

/// Execute a native write. Errors mean the caller must degrade the decision per D25.
pub fn apply_native_write(write: &NativeWrite) -> Result<(), String> {
    if !write.is_valid() {
        return Err("invalid native write".to_string());
    }
    match write {
        NativeWrite::McpApprovalMode {
            config_path,
            server,
            tool,
        } => write_mcp_approval_mode(Path::new(config_path), server, tool),
        NativeWrite::NetworkRule {
            rules_path,
            host,
            protocol,
        } => append_network_rule(Path::new(rules_path), host, protocol),
        NativeWrite::PrefixRule { rules_path, prefix } => {
            // D45: verify the rule against the installed Codex before touching the file.
            crate::permission_shell::verify_prefix_rule(prefix)?;
            append_prefix_rule(Path::new(rules_path), prefix)
        }
    }
}

/// Byte-exact replication of Codex `blocking_append_allow_prefix_rule`
/// (`execpolicy/src/amend.rs`): tokens JSON-serialized and joined by `, `.
fn append_prefix_rule(path: &Path, prefix: &[String]) -> Result<(), String> {
    let tokens = prefix
        .iter()
        .map(|token| {
            serde_json::to_string(token).map_err(|error| format!("serialize token: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let line = format!(
        r#"prefix_rule(pattern=[{}], decision="allow")"#,
        tokens.join(", ")
    );
    append_locked_rule_line(path, &line)
}

/// Byte-exact replication of Codex `blocking_append_network_rule`.
fn append_network_rule(path: &Path, host: &str, protocol: &str) -> Result<(), String> {
    let host_json =
        serde_json::to_string(host).map_err(|error| format!("serialize host: {error}"))?;
    let protocol_json =
        serde_json::to_string(protocol).map_err(|error| format!("serialize protocol: {error}"))?;
    let line =
        format!(r#"network_rule(host={host_json}, protocol={protocol_json}, decision="allow")"#);
    append_locked_rule_line(path, &line)
}

/// Replicates Codex `append_locked_line` (`execpolicy/src/amend.rs`): create the rules
/// directory, take an exclusive advisory lock on the file, dedup by whole line, append.
fn append_locked_rule_line(path: &Path, line: &str) -> Result<(), String> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let dir = path
        .parent()
        .ok_or_else(|| "rules path has no parent".to_string())?;
    std::fs::create_dir_all(dir).map_err(|error| format!("create rules dir: {error}"))?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(|error| format!("open rules file: {error}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        // Same advisory flock the native writer takes; blocks until available.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err("lock rules file failed".to_string());
        }
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("seek rules file: {error}"))?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|error| format!("read rules file: {error}"))?;
    if contents.lines().any(|existing| existing == line) {
        return Ok(());
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        file.write_all(b"\n")
            .map_err(|error| format!("write rules file: {error}"))?;
    }
    file.write_all(format!("{line}\n").as_bytes())
        .map_err(|error| format!("write rules file: {error}"))
}

/// Format-preserving `approval_mode = "approve"` edit mirroring Codex
/// `persist_custom_mcp_tool_approval` (`core/src/mcp_tool_call.rs`). The daemon never
/// creates servers: the `[mcp_servers.<server>]` entry must already exist in the file,
/// which also bounds what a compromised hook payload could make the daemon write.
fn write_mcp_approval_mode(path: &Path, server: &str, tool: &str) -> Result<(), String> {
    use toml_edit::{DocumentMut, Item};
    let text = std::fs::read_to_string(path).map_err(|error| format!("read: {error}"))?;
    let mut doc: DocumentMut = text.parse().map_err(|error| format!("parse: {error}"))?;
    let servers = doc
        .as_table_mut()
        .get_mut("mcp_servers")
        .and_then(Item::as_table_like_mut)
        .ok_or_else(|| "mcp_servers table not found".to_string())?;
    let server_item = servers
        .get_mut(server)
        .ok_or_else(|| format!("server `{server}` not defined"))?;
    set_approve_at(server_item, &["tools", tool])?;

    let tmp = path.with_extension(format!("toml.tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, doc.to_string().as_bytes()).map_err(|error| format!("write: {error}"))?;
    if let Ok(metadata) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, metadata.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|error| {
        let _ = std::fs::remove_file(&tmp);
        format!("rename: {error}")
    })
}

/// Descend through `keys` creating intermediate tables of the parent's kind (regular vs
/// inline, so the output stays valid TOML), then set `approval_mode = "approve"` on the
/// final table. Fails when an existing node on the way is not table-like.
fn set_approve_at(item: &mut toml_edit::Item, keys: &[&str]) -> Result<(), String> {
    use toml_edit::{InlineTable, Item, Table, Value};
    let Some(&key) = keys.first() else {
        let table = item
            .as_table_like_mut()
            .ok_or_else(|| "target is not a table".to_string())?;
        table.insert("approval_mode", toml_edit::value("approve"));
        return Ok(());
    };
    let parent_is_inline = matches!(item, Item::Value(_));
    let table = item
        .as_table_like_mut()
        .ok_or_else(|| format!("`{key}` parent is not a table"))?;
    if table.get(key).is_none() {
        let child = if parent_is_inline {
            Item::Value(Value::InlineTable(InlineTable::new()))
        } else {
            let mut child = Table::new();
            child.set_implicit(true);
            Item::Table(child)
        };
        table.insert(key, child);
    }
    let child = table
        .get_mut(key)
        .ok_or_else(|| format!("`{key}` missing after insert"))?;
    set_approve_at(child, &keys[1..])
}

/// Action id used when the daemon answers from stored rules without any surface.
pub const AUTO_ALLOW_ACTION_ID: &str = "memory_auto_allow";
/// Maximum rules a single save may carry (defensive bound on hook input).
pub const MAX_RULES_PER_SAVE: usize = 256;

impl PermissionMemory {
    /// Daemon-side validation against the confirm spec choices (fail closed on any mismatch).
    pub fn validate(&self, choice_ids: &[&str]) -> Result<(), String> {
        for save in &self.saves {
            if !choice_ids.contains(&save.action_id.as_str()) {
                return Err(format!(
                    "memory save references unknown action {}",
                    save.action_id
                ));
            }
            if save.action_id == AUTO_ALLOW_ACTION_ID {
                return Err("memory save may not shadow the auto-allow action".into());
            }
            if save.native.is_none() && save.rules.is_empty() {
                return Err("memory save is empty".into());
            }
            if save.rules.len() > MAX_RULES_PER_SAVE {
                return Err("memory save carries an invalid rule count".into());
            }
            if save.rules.iter().any(|rule| !rule.is_valid()) {
                return Err("memory save carries an invalid rule".into());
            }
            if save.native.as_ref().is_some_and(|write| !write.is_valid()) {
                return Err("memory save carries an invalid native write".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempStore {
        dir: PathBuf,
    }

    impl TempStore {
        fn new() -> Self {
            let dir =
                std::env::temp_dir().join(format!("ah-codex-rules-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }
        fn file(&self) -> PathBuf {
            self.dir.join("codex_rules.json")
        }
        fn lock(&self) -> PathBuf {
            self.dir.join("codex_rules.lock")
        }
    }

    impl Drop for TempStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn file_query(paths: &[&str]) -> MemoryQuery {
        MemoryQuery::FileEdit {
            paths: paths.iter().map(|p| p.to_string()).collect(),
        }
    }

    #[test]
    fn exact_file_rules_require_full_coverage() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[
                RuleKey::FileExact {
                    path: "/p/a.rs".into(),
                },
                RuleKey::FileExact {
                    path: "/p/b.rs".into(),
                },
            ],
        )
        .unwrap();
        assert!(check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/a.rs", "/p/b.rs"])
        ));
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/a.rs", "/p/c.rs"])
        ));
        // Other sessions never share rules (D6).
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s2",
            &file_query(&["/p/a.rs"])
        ));
        // Case-sensitive byte comparison (D46).
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/A.rs"])
        ));
    }

    #[test]
    fn project_rule_matches_lexical_prefix_at_component_boundary() {
        let t = TempStore::new();
        // Use a real temp dir so the symlink guard sees plain directories.
        let root = t.dir.join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let root_str = root.to_string_lossy().to_string();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileProject {
                root: root_str.clone(),
            }],
        )
        .unwrap();
        assert!(check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&[&format!("{root_str}/src/main.rs")])
        ));
        // Sibling directory sharing the prefix must not match.
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&[&format!("{root_str}-evil/x.rs")])
        ));
    }

    #[cfg(unix)]
    #[test]
    fn project_rule_fails_closed_on_symlink_component() {
        let t = TempStore::new();
        let root = t.dir.join("proj");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink("/etc", root.join("link")).unwrap();
        let root_str = root.to_string_lossy().to_string();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileProject {
                root: root_str.clone(),
            }],
        )
        .unwrap();
        assert!(check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&[&format!("{root_str}/real/a.txt")])
        ));
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&[&format!("{root_str}/link/passwd")])
        ));
    }

    #[test]
    fn disk_rule_covers_everything_in_session() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileDisk],
        )
        .unwrap();
        assert!(check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/anywhere/at/all.txt"])
        ));
    }

    #[test]
    fn mcp_rules_check_session_then_global_namespace() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "",
            RuleNamespace::Global,
            &[RuleKey::McpTool {
                tool: "mcp__github__create_issue".into(),
            }],
        )
        .unwrap();
        let query = MemoryQuery::McpTool {
            tool: "mcp__github__create_issue".into(),
        };
        // Global rules apply to any session (D41).
        assert!(check_auto_allow_at(&t.file(), &t.lock(), "s1", &query));
        assert!(check_auto_allow_at(&t.file(), &t.lock(), "s2", &query));
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &MemoryQuery::McpTool {
                tool: "mcp__github__delete_repo".into()
            }
        ));
    }

    #[test]
    fn shell_segments_all_need_exact_or_prefix_cover() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[
                RuleKey::ShellExact {
                    argv: vec!["git".into(), "status".into()],
                },
                RuleKey::ShellPrefix {
                    prefix: vec!["cargo".into()],
                },
            ],
        )
        .unwrap();
        let ok = MemoryQuery::ShellCommands {
            commands: vec![
                vec!["git".into(), "status".into()],
                vec!["cargo".into(), "build".into()],
            ],
            relaxed_eligible: false,
        };
        assert!(check_auto_allow_at(&t.file(), &t.lock(), "s1", &ok));
        let partial = MemoryQuery::ShellCommands {
            commands: vec![
                vec!["git".into(), "status".into()],
                vec!["rm".into(), "-rf".into(), "/".into()],
            ],
            relaxed_eligible: false,
        };
        assert!(!check_auto_allow_at(&t.file(), &t.lock(), "s1", &partial));
        // Exact match means full argv, not prefix.
        let longer = MemoryQuery::ShellCommands {
            commands: vec![vec!["git".into(), "status".into(), "--short".into()]],
            relaxed_eligible: false,
        };
        assert!(!check_auto_allow_at(&t.file(), &t.lock(), "s1", &longer));
    }

    #[test]
    fn relaxed_rule_is_session_scoped_and_refreshes_retention() {
        let t = TempStore::new();
        assert!(!check_relaxed_auto_allow_at(&t.file(), &t.lock(), "s1"));
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::ShellRelaxed],
        )
        .unwrap();
        assert!(check_relaxed_auto_allow_at(&t.file(), &t.lock(), "s1"));
        // Other sessions are unaffected; empty session ids never match.
        assert!(!check_relaxed_auto_allow_at(&t.file(), &t.lock(), "s2"));
        assert!(!check_relaxed_auto_allow_at(&t.file(), &t.lock(), ""));
        // The relaxed rule never satisfies command queries by itself.
        let query = MemoryQuery::ShellCommands {
            commands: vec![vec!["ls".into()]],
            relaxed_eligible: true,
        };
        assert!(!check_auto_allow_at(&t.file(), &t.lock(), "s1", &query));
    }

    #[test]
    fn yolo_rule_checks_disables_and_lists_per_session() {
        let t = TempStore::new();
        let check = |sid: &str| check_session_flag_at(&t.file(), &t.lock(), sid, &RuleKey::Yolo);
        assert!(!check("s1"));
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::Yolo, RuleKey::ShellRelaxed],
        )
        .unwrap();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s2",
            RuleNamespace::Session,
            &[RuleKey::Yolo],
        )
        .unwrap();
        assert!(check("s1"));
        assert!(!check("s3"));
        assert!(!check(""));
        // The yolo rule never satisfies command queries by itself.
        let query = MemoryQuery::ShellCommands {
            commands: vec![vec!["ls".into()]],
            relaxed_eligible: false,
        };
        assert!(!check_auto_allow_at(&t.file(), &t.lock(), "s1", &query));
        // Listing covers exactly the sessions with a yolo rule.
        let mut ids = yolo_session_ids_at(&t.file());
        ids.sort();
        assert_eq!(ids, ["s1", "s2"]);
        // Summary flags yolo without counting it as a shell rule.
        let summaries = session_summaries_at(&t.file());
        let s1 = summaries.iter().find(|s| s.session_id == "s1").unwrap();
        assert!(s1.yolo);
        // shell_count covers the relaxed rule only.
        assert_eq!(s1.shell_count, 1);
        // Disable removes only the yolo rule and keeps the rest.
        assert!(disable_yolo_at(&t.file(), &t.lock(), "s1"));
        assert!(!disable_yolo_at(&t.file(), &t.lock(), "s1"));
        assert!(!check("s1"));
        assert!(check_relaxed_auto_allow_at(&t.file(), &t.lock(), "s1"));
        assert_eq!(yolo_session_ids_at(&t.file()), ["s2"]);
    }

    #[test]
    fn network_rule_matches_host_protocol_port() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::NetworkHost {
                host: "api.github.com".into(),
                protocol: "https".into(),
                port: 443,
            }],
        )
        .unwrap();
        let hit = MemoryQuery::NetworkHost {
            host: "api.github.com".into(),
            protocol: "https".into(),
            port: 443,
        };
        assert!(check_auto_allow_at(&t.file(), &t.lock(), "s1", &hit));
        let other_port = MemoryQuery::NetworkHost {
            host: "api.github.com".into(),
            protocol: "https".into(),
            port: 8443,
        };
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &other_port
        ));
    }

    #[test]
    fn hits_refresh_last_used_and_expiry_prunes() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileExact {
                path: "/p/a.rs".into(),
            }],
        )
        .unwrap();
        // Backdate beyond the TTL: pruned, no match.
        let mut data = load_at(&t.file());
        let stale = now_ms() - RULE_TTL_MS - 1000;
        data.sessions.get_mut("s1").unwrap()[0].created_at_ms = stale;
        data.sessions.get_mut("s1").unwrap()[0].last_used_at_ms = stale;
        store_at(&t.file(), data);
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/a.rs"])
        ));
        assert!(load_at(&t.file()).sessions.is_empty());

        // A hit refreshes last_used_at.
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileExact {
                path: "/p/a.rs".into(),
            }],
        )
        .unwrap();
        let mut data = load_at(&t.file());
        let old = now_ms() - RULE_TTL_MS / 2;
        data.sessions.get_mut("s1").unwrap()[0].last_used_at_ms = old;
        store_at(&t.file(), data);
        assert!(check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/a.rs"])
        ));
        let refreshed = load_at(&t.file()).sessions["s1"][0].last_used_at_ms;
        assert!(refreshed > old);
    }

    #[test]
    fn capacity_rejects_new_rules_without_evicting() {
        let t = TempStore::new();
        let keys: Vec<RuleKey> = (0..SESSION_RULE_CAP)
            .map(|index| RuleKey::FileExact {
                path: format!("/p/f{index}.rs"),
            })
            .collect();
        save_rules_at(&t.file(), &t.lock(), "s1", RuleNamespace::Session, &keys).unwrap();
        let error = save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileExact {
                path: "/p/one-more.rs".into(),
            }],
        )
        .unwrap_err();
        assert_eq!(error, SaveError::CapacityExceeded);
        // Existing rules stay intact and re-saving an existing key still succeeds.
        assert_eq!(load_at(&t.file()).sessions["s1"].len(), SESSION_RULE_CAP);
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileExact {
                path: "/p/f0.rs".into(),
            }],
        )
        .unwrap();
    }

    #[test]
    fn save_dedups_and_validates() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[
                RuleKey::FileExact {
                    path: "/p/a.rs".into(),
                },
                RuleKey::FileExact {
                    path: "/p/a.rs".into(),
                },
            ],
        )
        .unwrap();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileExact {
                path: "/p/a.rs".into(),
            }],
        )
        .unwrap();
        assert_eq!(load_at(&t.file()).sessions["s1"].len(), 1);
        assert_eq!(
            save_rules_at(
                &t.file(),
                &t.lock(),
                "s1",
                RuleNamespace::Session,
                &[RuleKey::FileExact {
                    path: "relative/path".into()
                }]
            )
            .unwrap_err(),
            SaveError::InvalidRule
        );
        assert_eq!(
            save_rules_at(
                &t.file(),
                &t.lock(),
                " ",
                RuleNamespace::Session,
                &[RuleKey::FileDisk]
            )
            .unwrap_err(),
            SaveError::InvalidRule
        );
    }

    #[test]
    fn reset_session_and_global_are_scoped() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileDisk],
        )
        .unwrap();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "",
            RuleNamespace::Global,
            &[RuleKey::McpTool {
                tool: "mcp__a__b".into(),
            }],
        )
        .unwrap();
        assert_eq!(reset_session_at(&t.file(), &t.lock(), "s1"), 1);
        assert_eq!(reset_session_at(&t.file(), &t.lock(), "s1"), 0);
        assert_eq!(load_at(&t.file()).global.len(), 1);
        assert_eq!(reset_global_at(&t.file(), &t.lock()), 1);
        assert_eq!(load_at(&t.file()).global.len(), 0);
    }

    #[test]
    fn summaries_group_by_kind() {
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[
                RuleKey::FileExact {
                    path: "/p/a.rs".into(),
                },
                RuleKey::FileProject { root: "/p".into() },
                RuleKey::NetworkHost {
                    host: "example.com".into(),
                    protocol: "https".into(),
                    port: 443,
                },
                RuleKey::McpTool {
                    tool: "mcp__a__b".into(),
                },
            ],
        )
        .unwrap();
        let summaries = session_summaries_at(&t.file());
        assert_eq!(summaries.len(), 1);
        let summary = &summaries[0];
        assert_eq!(summary.session_id, "s1");
        assert_eq!(summary.rule_count, 4);
        assert_eq!(summary.file_exact_count, 1);
        assert_eq!(summary.project_roots, vec!["/p"]);
        assert!(!summary.full_disk);
        assert_eq!(summary.network_count, 1);
        assert_eq!(summary.mcp_count, 1);
        assert!(summary.last_used_at_ms > 0);
    }

    #[test]
    fn network_rule_append_matches_native_format_and_dedups() {
        let t = TempStore::new();
        let rules = t.dir.join("rules/default.rules");
        let write = NativeWrite::NetworkRule {
            rules_path: rules.to_string_lossy().to_string(),
            host: "api.github.com".into(),
            protocol: "https".into(),
        };
        apply_native_write(&write).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rules).unwrap(),
            "network_rule(host=\"api.github.com\", protocol=\"https\", decision=\"allow\")\n"
        );
        // Whole-line dedup, same as append_locked_line.
        apply_native_write(&write).unwrap();
        assert_eq!(std::fs::read_to_string(&rules).unwrap().lines().count(), 1);
        // Existing content without a trailing newline gets one before the append.
        std::fs::write(&rules, "prefix_rule(pattern=[\"ls\"], decision=\"allow\")").unwrap();
        apply_native_write(&write).unwrap();
        assert_eq!(
            std::fs::read_to_string(&rules).unwrap(),
            "prefix_rule(pattern=[\"ls\"], decision=\"allow\")\nnetwork_rule(host=\"api.github.com\", protocol=\"https\", decision=\"allow\")\n"
        );

        // Structural validation rejects wrong paths, hosts and protocols outright.
        for bad in [
            NativeWrite::NetworkRule {
                rules_path: "/etc/rules/other.rules".into(),
                host: "h".into(),
                protocol: "https".into(),
            },
            NativeWrite::NetworkRule {
                rules_path: rules.to_string_lossy().to_string(),
                host: "*.example.com".into(),
                protocol: "https".into(),
            },
            NativeWrite::NetworkRule {
                rules_path: rules.to_string_lossy().to_string(),
                host: "example.com".into(),
                protocol: "ftp".into(),
            },
        ] {
            assert!(!bad.is_valid());
            assert!(apply_native_write(&bad).is_err());
        }
    }

    #[test]
    fn prefix_rule_append_matches_native_format_and_validates() {
        let t = TempStore::new();
        let rules = t.dir.join("rules/default.rules");
        append_prefix_rule(&rules, &["echo".to_string(), "Hello, world!".to_string()]).unwrap();
        // Byte-exact against the blocking_append_allow_prefix_rule fixture.
        assert_eq!(
            std::fs::read_to_string(&rules).unwrap(),
            "prefix_rule(pattern=[\"echo\", \"Hello, world!\"], decision=\"allow\")\n"
        );
        // Whole-line dedup.
        append_prefix_rule(&rules, &["echo".to_string(), "Hello, world!".to_string()]).unwrap();
        assert_eq!(std::fs::read_to_string(&rules).unwrap().lines().count(), 1);

        // Structural validation: banned prefixes and malformed paths are rejected.
        let valid = NativeWrite::PrefixRule {
            rules_path: rules.to_string_lossy().to_string(),
            prefix: vec!["cargo".into(), "build".into()],
        };
        assert!(valid.is_valid());
        for bad in [
            NativeWrite::PrefixRule {
                rules_path: rules.to_string_lossy().to_string(),
                prefix: vec!["git".into()],
            },
            NativeWrite::PrefixRule {
                rules_path: rules.to_string_lossy().to_string(),
                prefix: vec![],
            },
            NativeWrite::PrefixRule {
                rules_path: "/etc/somewhere/else.rules".into(),
                prefix: vec!["cargo".into()],
            },
        ] {
            assert!(!bad.is_valid());
            assert!(apply_native_write(&bad).is_err());
        }
    }

    #[test]
    fn normalize_path_is_lexical_and_cwd_based() {
        assert_eq!(
            normalize_path("src/../a.rs", "/work/proj").as_deref(),
            Some("/work/proj/a.rs")
        );
        assert_eq!(
            normalize_path("./src/main.rs", "/work/proj/").as_deref(),
            Some("/work/proj/src/main.rs")
        );
        assert_eq!(
            normalize_path("/abs/./x/../y.txt", "/ignored").as_deref(),
            Some("/abs/y.txt")
        );
        // `..` above root clamps at root.
        assert_eq!(
            normalize_path("../../../../etc/passwd", "/a").as_deref(),
            Some("/etc/passwd")
        );
        assert!(normalize_path("", "/a").is_none());
        assert!(normalize_path("a.txt", "relative-cwd").is_none());
        // No filesystem access: non-existent paths still normalize.
        assert_eq!(
            normalize_path("/no/such/dir/file.txt", "/").as_deref(),
            Some("/no/such/dir/file.txt")
        );
    }

    #[test]
    fn memory_validation_fails_closed() {
        let memory = PermissionMemory {
            query: None,
            saves: vec![MemorySave {
                action_id: "remember_files".into(),
                namespace: RuleNamespace::Session,
                rules: vec![RuleKey::FileExact {
                    path: "/p/a.rs".into(),
                }],
                native: None,
            }],
        };
        assert!(memory
            .validate(&["approve_once", "remember_files", "deny"])
            .is_ok());
        assert!(memory.validate(&["approve_once", "deny"]).is_err());
        let bad_rule = PermissionMemory {
            query: None,
            saves: vec![MemorySave {
                action_id: "remember_files".into(),
                namespace: RuleNamespace::Session,
                rules: vec![],
                native: None,
            }],
        };
        assert!(bad_rule.validate(&["remember_files"]).is_err());
        // A native-only save (no shadow rules) is legal; a malformed native write is not.
        let native_only = PermissionMemory {
            query: None,
            saves: vec![MemorySave {
                action_id: "remember_mcp_always".into(),
                namespace: RuleNamespace::Session,
                rules: vec![],
                native: Some(NativeWrite::McpApprovalMode {
                    config_path: "/home/u/.codex/config.toml".into(),
                    server: "github".into(),
                    tool: "create_issue".into(),
                }),
            }],
        };
        assert!(native_only.validate(&["remember_mcp_always"]).is_ok());
        let bad_native = PermissionMemory {
            query: None,
            saves: vec![MemorySave {
                action_id: "remember_mcp_always".into(),
                namespace: RuleNamespace::Session,
                rules: vec![],
                native: Some(NativeWrite::McpApprovalMode {
                    config_path: "/etc/passwd".into(),
                    server: "github".into(),
                    tool: "create_issue".into(),
                }),
            }],
        };
        assert!(bad_native.validate(&["remember_mcp_always"]).is_err());
    }

    #[test]
    fn native_mcp_write_is_format_preserving_and_fails_closed() {
        let t = TempStore::new();
        let config = t.dir.join("config.toml");
        std::fs::write(
            &config,
            "# my config\nmodel = \"gpt-5\"\n\n[mcp_servers.github]\ncommand = \"gh-mcp\"\n",
        )
        .unwrap();
        apply_native_write(&NativeWrite::McpApprovalMode {
            config_path: config.to_string_lossy().to_string(),
            server: "github".into(),
            tool: "create_issue".into(),
        })
        .unwrap();
        let text = std::fs::read_to_string(&config).unwrap();
        // Original content and comments survive the edit.
        assert!(text.contains("# my config"));
        assert!(text.contains("model = \"gpt-5\""));
        assert!(text.contains("command = \"gh-mcp\""));
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["github"]["tools"]["create_issue"]["approval_mode"].as_str(),
            Some("approve")
        );

        // Unknown server: refused, file untouched.
        let before = std::fs::read_to_string(&config).unwrap();
        assert!(apply_native_write(&NativeWrite::McpApprovalMode {
            config_path: config.to_string_lossy().to_string(),
            server: "nope".into(),
            tool: "x".into(),
        })
        .is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), before);

        // Inline-table server definitions stay valid TOML after the edit.
        std::fs::create_dir_all(t.dir.join("inline")).unwrap();
        let inline = t.dir.join("inline/config.toml");
        std::fs::write(
            &inline,
            "mcp_servers = { github = { command = \"gh-mcp\" } }\n",
        )
        .unwrap();
        apply_native_write(&NativeWrite::McpApprovalMode {
            config_path: inline.to_string_lossy().to_string(),
            server: "github".into(),
            tool: "create_issue".into(),
        })
        .unwrap();
        let doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&inline).unwrap().parse().unwrap();
        assert_eq!(
            doc["mcp_servers"]["github"]["tools"]["create_issue"]["approval_mode"].as_str(),
            Some("approve")
        );
    }

    #[test]
    fn corrupt_store_degrades_to_empty() {
        let t = TempStore::new();
        std::fs::write(t.file(), "not json").unwrap();
        assert!(!check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/a.rs"])
        ));
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileDisk],
        )
        .unwrap();
        assert!(check_auto_allow_at(
            &t.file(),
            &t.lock(),
            "s1",
            &file_query(&["/p/a.rs"])
        ));
    }

    #[cfg(unix)]
    #[test]
    fn store_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let t = TempStore::new();
        save_rules_at(
            &t.file(),
            &t.lock(),
            "s1",
            RuleNamespace::Session,
            &[RuleKey::FileDisk],
        )
        .unwrap();
        let mode = std::fs::metadata(t.file()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}
