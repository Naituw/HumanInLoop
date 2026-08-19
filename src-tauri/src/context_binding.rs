//! Short-lived one-time bindings used to carry a native Agent session through MCP tool input.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::agents::AgentKind;

pub const HIDDEN_TOKEN_FIELD: &str = "__askhuman_session_token_v1";
const TOKEN_TTL_MS: i64 = 30_000;
const GROK_PENDING_TTL_MS: i64 = 5_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TokenRecord {
    agent_kind: String,
    session_id: String,
    tool_name: String,
    expires_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBinding {
    pub agent_kind: String,
    pub session_id: String,
}

#[derive(Debug, Clone)]
struct McpInstance {
    project: String,
    server_pid: u32,
    agent_pid: u32,
}

#[derive(Debug, Clone)]
struct GrokPending {
    mcp_instance_id: String,
    agent_session_id: String,
    qualified_tool_name: String,
    arguments_sha256: String,
    project: String,
    tool_use_id: Option<String>,
    created_at_ms: i64,
}

#[derive(Default)]
struct GrokInner {
    instances: HashMap<String, McpInstance>,
    pending: Vec<GrokPending>,
}

#[derive(Default)]
pub struct GrokBindingRegistry {
    inner: Mutex<GrokInner>,
}

impl GrokBindingRegistry {
    pub fn register(
        &self,
        mcp_instance_id: String,
        project: String,
        server_pid: u32,
        parent_pid_hint: Option<u32>,
    ) {
        if uuid::Uuid::parse_str(&mcp_instance_id).is_err() || project.is_empty() {
            return;
        }
        let start = parent_pid_hint.unwrap_or(server_pid);
        let Some(agent_pid) = crate::agents::detect::walk_agent_pid(AgentKind::Grok, start)
            .or_else(|| crate::agents::detect::walk_agent_pid(AgentKind::Grok, server_pid))
        else {
            return;
        };
        self.register_resolved(mcp_instance_id, project, server_pid, agent_pid);
    }

    fn register_resolved(
        &self,
        mcp_instance_id: String,
        project: String,
        server_pid: u32,
        agent_pid: u32,
    ) {
        if uuid::Uuid::parse_str(&mcp_instance_id).is_err()
            || project.is_empty()
            || !crate::agents::detect::pid_alive(server_pid)
        {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        clean_grok(&mut inner);
        inner.instances.insert(
            mcp_instance_id,
            McpInstance {
                project,
                server_pid,
                agent_pid,
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_pending(
        &self,
        agent_session_id: String,
        qualified_tool_name: String,
        arguments_sha256: String,
        project: String,
        hook_parent_hint: Option<u32>,
        tool_use_id: Option<String>,
        created_at_ms: i64,
    ) {
        if agent_session_id.trim().is_empty()
            || project.is_empty()
            || !matches!(
                qualified_tool_name.as_str(),
                "askhuman__ask" | "askhuman__whats_next" | "askhuman__show_last"
            )
        {
            return;
        }
        let Some(agent_pid) = hook_parent_hint
            .and_then(|pid| crate::agents::detect::walk_agent_pid(AgentKind::Grok, pid))
        else {
            return;
        };
        self.add_pending_resolved(
            agent_session_id,
            qualified_tool_name,
            arguments_sha256,
            project,
            agent_pid,
            tool_use_id,
            created_at_ms,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn add_pending_resolved(
        &self,
        agent_session_id: String,
        qualified_tool_name: String,
        arguments_sha256: String,
        project: String,
        agent_pid: u32,
        tool_use_id: Option<String>,
        created_at_ms: i64,
    ) {
        if agent_session_id.trim().is_empty()
            || project.is_empty()
            || !matches!(
                qualified_tool_name.as_str(),
                "askhuman__ask" | "askhuman__whats_next" | "askhuman__show_last"
            )
        {
            return;
        }
        let mut inner = self.inner.lock().unwrap();
        clean_grok(&mut inner);
        let matching: Vec<String> = inner
            .instances
            .iter()
            .filter(|(_, instance)| {
                instance.project == project
                    && instance.agent_pid == agent_pid
                    && crate::agents::detect::pid_alive(instance.server_pid)
            })
            .map(|(id, _)| id.clone())
            .collect();
        // Partition permanently only when process+project identify exactly one MCP instance.
        if matching.len() != 1 {
            return;
        }
        inner.pending.push(GrokPending {
            mcp_instance_id: matching[0].clone(),
            agent_session_id,
            qualified_tool_name,
            arguments_sha256,
            project,
            tool_use_id,
            created_at_ms,
        });
    }

    pub fn claim(
        &self,
        mcp_instance_id: &str,
        project: &str,
        tool_name: &str,
        arguments_sha256: &str,
        server_pid: u32,
    ) -> Option<String> {
        let qualified = format!("askhuman__{tool_name}");
        let mut inner = self.inner.lock().unwrap();
        clean_grok(&mut inner);
        let instance = inner.instances.get(mcp_instance_id)?;
        if instance.project != project
            || instance.server_pid != server_pid
            || !crate::agents::detect::pid_alive(server_pid)
        {
            return None;
        }
        let matches: Vec<usize> = inner
            .pending
            .iter()
            .enumerate()
            .filter(|(_, pending)| {
                pending.mcp_instance_id == mcp_instance_id
                    && pending.project == project
                    && pending.qualified_tool_name == qualified
                    && pending.arguments_sha256 == arguments_sha256
            })
            .map(|(index, _)| index)
            .collect();
        if matches.len() != 1 {
            return None;
        }
        Some(inner.pending.remove(matches[0]).agent_session_id)
    }
}

fn clean_grok(inner: &mut GrokInner) {
    let now = crate::history::now_ms();
    inner
        .instances
        .retain(|_, instance| crate::agents::detect::pid_alive(instance.server_pid));
    inner.pending.retain(|pending| {
        let _ = pending.tool_use_id.as_deref();
        now.saturating_sub(pending.created_at_ms) <= GROK_PENDING_TTL_MS
            && inner.instances.contains_key(&pending.mcp_instance_id)
    });
}

/// Versioned canonical JSON hash shared by Grok's hook and MCP handler.
pub fn canonical_arguments_sha256(arguments: &Value) -> String {
    fn canonical(value: &Value, output: &mut String) {
        match value {
            Value::Object(object) => {
                let mut keys: Vec<&String> = object.keys().collect();
                keys.sort();
                output.push('{');
                for (index, key) in keys.into_iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    output.push_str(&serde_json::to_string(key).unwrap());
                    output.push(':');
                    canonical(&object[key], output);
                }
                output.push('}');
            }
            Value::Array(array) => {
                output.push('[');
                for (index, item) in array.iter().enumerate() {
                    if index > 0 {
                        output.push(',');
                    }
                    canonical(item, output);
                }
                output.push(']');
            }
            other => output.push_str(&serde_json::to_string(other).unwrap()),
        }
    }
    let mut canonical_json = String::from("askhuman-args-v1:");
    canonical(arguments, &mut canonical_json);
    format!("{:x}", Sha256::digest(canonical_json.as_bytes()))
}

/// Normalize raw hook arguments through the same typed shapes used by the MCP handler before
/// hashing. This keeps omitted nested defaults (for example `recommended: false`) identical on
/// both sides of Grok's side channel.
pub fn canonical_tool_arguments_sha256(tool_name: &str, arguments: &Value) -> Option<String> {
    let normalized = match tool_name {
        "ask" => {
            let params =
                serde_json::from_value::<crate::mcp::ask::AskParams>(arguments.clone()).ok()?;
            crate::mcp::ask::ask_arguments_value(&params)
        }
        "whats_next" => {
            let params =
                serde_json::from_value::<crate::mcp::ask::WhatsNextParams>(arguments.clone())
                    .ok()?;
            crate::mcp::ask::whats_next_arguments_value(&params)
        }
        "show_last" => {
            let params =
                serde_json::from_value::<crate::mcp::ask::ShowLastParams>(arguments.clone())
                    .ok()?;
            // Reject unknown public keys by requiring deserialize + only known fields.
            if let Some(object) = arguments.as_object() {
                if object
                    .keys()
                    .any(|key| key != "count" && !key.starts_with("__askhuman_"))
                {
                    return None;
                }
            }
            crate::mcp::ask::show_last_arguments_value(&params)
        }
        _ => return None,
    };
    Some(canonical_arguments_sha256(&normalized))
}

pub fn record_grok_pending(
    session_id: &str,
    qualified_tool_name: &str,
    tool_name: &str,
    arguments: &Value,
    hook_input: &Value,
    env: &HashMap<String, String>,
) {
    let project = super_project(env, hook_input);
    if project.is_empty() {
        return;
    }
    let Some(arguments_sha256) = canonical_tool_arguments_sha256(tool_name, arguments) else {
        return;
    };
    let tool_use_id = ["tool_use_id", "toolUseId"]
        .into_iter()
        .find_map(|key| hook_input.get(key).and_then(Value::as_str))
        .map(str::to_string);
    let hook_parent_hint = crate::agents::detect::parent_pid(std::process::id());
    crate::client::report_grok_binding_pending(crate::ipc::ClientMsg::GrokBindingPending {
        agent_session_id: session_id.to_string(),
        qualified_tool_name: qualified_tool_name.to_string(),
        arguments_sha256,
        project,
        hook_parent_hint,
        tool_use_id,
        created_at_ms: crate::history::now_ms(),
    });
}

fn super_project(env: &HashMap<String, String>, input: &Value) -> String {
    let cwd = crate::agents::report::resolve_cwd(env, Some(input))
        .map(PathBuf::from)
        .unwrap_or_default();
    if cwd.as_os_str().is_empty() {
        String::new()
    } else {
        crate::project::detect_from(&cwd)
    }
}

/// Create a private 128-bit random token. The native session id never enters the Agent transcript.
pub fn create_token(agent_kind: &str, session_id: &str, tool_name: &str) -> Option<String> {
    create_token_at(
        &crate::paths::session_token_dir(),
        agent_kind,
        session_id,
        tool_name,
    )
}

fn create_token_at(
    token_dir: &Path,
    agent_kind: &str,
    session_id: &str,
    tool_name: &str,
) -> Option<String> {
    if crate::agents::AgentKind::parse(agent_kind).is_none()
        || session_id.trim().is_empty()
        || !is_supported_tool(tool_name)
    {
        return None;
    }
    let token = uuid::Uuid::new_v4().to_string();
    let path = token_path_at(token_dir, &token)?;
    let record = TokenRecord {
        agent_kind: agent_kind.to_string(),
        session_id: session_id.to_string(),
        tool_name: tool_name.to_string(),
        expires_at_ms: crate::history::now_ms() + TOKEN_TTL_MS,
    };
    let bytes = serde_json::to_vec(&record).ok()?;
    crate::integrations::hook_edit::atomic_write_private(&path, &bytes).ok()?;
    cleanup_expired_at(token_dir);
    Some(token)
}

/// Atomically consume a token. Invalid, expired, replayed, or wrong-tool tokens yield no binding.
pub fn consume_token(token: &str, expected_tool: &str) -> Option<AgentBinding> {
    consume_token_at(&crate::paths::session_token_dir(), token, expected_tool)
}

fn consume_token_at(token_dir: &Path, token: &str, expected_tool: &str) -> Option<AgentBinding> {
    if !is_supported_tool(expected_tool) {
        return None;
    }
    let path = token_path_at(token_dir, token)?;
    let claimed = path.with_extension("claim");
    // Windows can let concurrent renames of one source all succeed. CREATE_NEW is the portable
    // exclusive operation here, so hold the claim file until the token path has been removed.
    let claim_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&claimed)
        .ok()?;
    let bytes = std::fs::read(&path).ok();
    let _ = std::fs::remove_file(&path);
    drop(claim_file);
    let _ = std::fs::remove_file(&claimed);
    let result = bytes
        .and_then(|bytes| serde_json::from_slice::<TokenRecord>(&bytes).ok())
        .filter(|record| {
            record.expires_at_ms >= crate::history::now_ms()
                && record.tool_name == expected_tool
                && crate::agents::AgentKind::parse(&record.agent_kind).is_some()
                && !record.session_id.trim().is_empty()
        })
        .map(|record| AgentBinding {
            agent_kind: record.agent_kind,
            session_id: record.session_id,
        });
    cleanup_expired_at(token_dir);
    result
}

fn is_supported_tool(name: &str) -> bool {
    matches!(name, "ask" | "whats_next" | "show_last")
}

fn token_path_at(token_dir: &Path, token: &str) -> Option<PathBuf> {
    let parsed = uuid::Uuid::parse_str(token).ok()?;
    Some(token_dir.join(format!("{}.json", parsed.hyphenated())))
}

fn cleanup_expired_at(token_dir: &Path) {
    let now = crate::history::now_ms();
    let Ok(entries) = std::fs::read_dir(token_dir) else {
        return;
    };
    for entry in entries.flatten().take(256) {
        let path = entry.path();
        let extension = path.extension().and_then(|value| value.to_str());
        let expired = if extension == Some("json") {
            std::fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<TokenRecord>(&bytes).ok())
                .map(|record| record.expires_at_ms < now)
                .unwrap_or(true)
        } else if extension.is_some_and(|value| value == "claim" || value.starts_with("claim-")) {
            std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_some_and(|age| age.as_millis() > TOKEN_TTL_MS as u128)
        } else {
            false
        };
        if expired {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_single_use_and_tool_bound() {
        let dir = tempfile::tempdir().unwrap();

        let wrong = create_token_at(dir.path(), "claude", "session", "ask").unwrap();
        assert!(consume_token_at(dir.path(), &wrong, "show_last").is_none());
        assert!(consume_token_at(dir.path(), &wrong, "ask").is_none());

        let token = create_token_at(dir.path(), "cursor", "conversation", "show_last").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let token_path = token_path_at(dir.path(), &token).unwrap();
            assert_eq!(
                std::fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                std::fs::metadata(&token_path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(
            consume_token_at(dir.path(), &token, "show_last"),
            Some(AgentBinding {
                agent_kind: "cursor".into(),
                session_id: "conversation".into()
            })
        );
        assert!(consume_token_at(dir.path(), &token, "show_last").is_none());

        let expired = create_token_at(dir.path(), "claude", "old", "ask").unwrap();
        let expired_path = token_path_at(dir.path(), &expired).unwrap();
        let mut record: TokenRecord =
            serde_json::from_slice(&std::fs::read(&expired_path).unwrap()).unwrap();
        record.expires_at_ms = crate::history::now_ms() - 1;
        crate::integrations::hook_edit::atomic_write_private(
            &expired_path,
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        assert!(consume_token_at(dir.path(), &expired, "ask").is_none());
    }

    #[test]
    fn token_creation_and_paths_reject_untrusted_shapes() {
        let dir = tempfile::tempdir().unwrap();
        assert!(create_token_at(dir.path(), "unknown", "session", "ask").is_none());
        assert!(create_token_at(dir.path(), "claude", "   ", "ask").is_none());
        assert!(create_token_at(dir.path(), "claude", "session", "other").is_none());
        for token in ["", "../escape", "not-a-uuid"] {
            assert!(consume_token_at(dir.path(), token, "ask").is_none());
        }
        assert!(
            consume_token_at(dir.path(), "5c2bfe55-f587-4f0f-9671-f02d5259f2e1", "other").is_none()
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn concurrent_token_consumers_have_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        for round in 0..32 {
            let token = create_token_at(dir.path(), "claude", "session", "ask").unwrap();
            let token_dir = dir.path().to_path_buf();
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let token_dir = token_dir.clone();
                    let token = token.clone();
                    std::thread::spawn(move || consume_token_at(&token_dir, &token, "ask"))
                })
                .collect();
            let winners: Vec<_> = handles
                .into_iter()
                .filter_map(|handle| handle.join().unwrap())
                .collect();
            assert_eq!(winners.len(), 1, "round {round}");
            assert_eq!(winners[0].session_id, "session");
        }
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn canonical_arguments_hash_sorts_object_keys() {
        let a = serde_json::json!({"b": 2, "a": {"y": 1, "x": 0}});
        let b = serde_json::json!({"a": {"x": 0, "y": 1}, "b": 2});
        assert_eq!(
            canonical_arguments_sha256(&a),
            canonical_arguments_sha256(&b)
        );
    }

    #[test]
    fn grok_hash_normalizes_nested_mcp_defaults() {
        let raw = serde_json::json!({
            "questions": [{
                "question": "Continue?",
                "options": [{"text": "Yes"}]
            }]
        });
        let handler_shape = serde_json::json!({
            "questions": [{
                "question": "Continue?",
                "options": [{"text": "Yes", "recommended": false}]
            }]
        });
        assert_eq!(
            canonical_tool_arguments_sha256("ask", &raw),
            canonical_tool_arguments_sha256("ask", &handler_shape)
        );
        assert_eq!(
            canonical_tool_arguments_sha256("whats_next", &serde_json::json!({})),
            Some(canonical_arguments_sha256(&serde_json::json!({})))
        );
        assert_eq!(
            canonical_tool_arguments_sha256("show_last", &serde_json::json!({})),
            Some(canonical_arguments_sha256(&serde_json::json!({})))
        );
        assert_eq!(
            canonical_tool_arguments_sha256("show_last", &serde_json::json!({"count": 3})),
            Some(canonical_arguments_sha256(&serde_json::json!({"count": 3})))
        );
        assert!(canonical_tool_arguments_sha256(
            "show_last",
            &serde_json::json!({"unexpected": true})
        )
        .is_none());
        assert!(canonical_tool_arguments_sha256("unknown", &serde_json::json!({})).is_none());
    }

    #[test]
    fn grok_claim_requires_one_candidate_in_exact_instance_partition() {
        let registry = GrokBindingRegistry::default();
        let now = crate::history::now_ms();
        let pid = std::process::id();
        {
            let mut inner = registry.inner.lock().unwrap();
            inner.instances.insert(
                "instance-1".into(),
                McpInstance {
                    project: "/p".into(),
                    server_pid: pid,
                    agent_pid: 42,
                },
            );
            inner.instances.insert(
                "instance-2".into(),
                McpInstance {
                    project: "/p".into(),
                    server_pid: pid,
                    agent_pid: 42,
                },
            );
            inner.pending.push(GrokPending {
                mcp_instance_id: "instance-1".into(),
                agent_session_id: "session-1".into(),
                qualified_tool_name: "askhuman__show_last".into(),
                arguments_sha256: "hash".into(),
                project: "/p".into(),
                tool_use_id: Some("tool-1".into()),
                created_at_ms: now,
            });
        }
        assert!(registry
            .claim("instance-2", "/p", "show_last", "hash", pid)
            .is_none());
        assert_eq!(
            registry.claim("instance-1", "/p", "show_last", "hash", pid),
            Some("session-1".into())
        );

        let mut inner = registry.inner.lock().unwrap();
        for session in ["s2", "s3"] {
            inner.pending.push(GrokPending {
                mcp_instance_id: "instance-1".into(),
                agent_session_id: session.into(),
                qualified_tool_name: "askhuman__ask".into(),
                arguments_sha256: "same".into(),
                project: "/p".into(),
                tool_use_id: None,
                created_at_ms: now,
            });
        }
        drop(inner);
        assert!(registry
            .claim("instance-1", "/p", "ask", "same", pid)
            .is_none());
    }

    #[test]
    fn grok_claim_rejects_every_mismatched_or_expired_dimension() {
        fn registry_with(created_at_ms: i64) -> GrokBindingRegistry {
            let registry = GrokBindingRegistry::default();
            let mut inner = registry.inner.lock().unwrap();
            inner.instances.insert(
                "instance".into(),
                McpInstance {
                    project: "/p".into(),
                    server_pid: std::process::id(),
                    agent_pid: 42,
                },
            );
            inner.pending.push(GrokPending {
                mcp_instance_id: "instance".into(),
                agent_session_id: "session".into(),
                qualified_tool_name: "askhuman__ask".into(),
                arguments_sha256: "hash".into(),
                project: "/p".into(),
                tool_use_id: Some("tool".into()),
                created_at_ms,
            });
            drop(inner);
            registry
        }

        let now = crate::history::now_ms();
        for (instance, project, tool, hash, pid) in [
            ("other", "/p", "ask", "hash", std::process::id()),
            ("instance", "/other", "ask", "hash", std::process::id()),
            ("instance", "/p", "show_last", "hash", std::process::id()),
            ("instance", "/p", "ask", "other", std::process::id()),
            ("instance", "/p", "ask", "hash", std::process::id() + 1),
        ] {
            assert!(registry_with(now)
                .claim(instance, project, tool, hash, pid)
                .is_none());
        }
        assert!(registry_with(now - GROK_PENDING_TTL_MS - 1)
            .claim("instance", "/p", "ask", "hash", std::process::id())
            .is_none());

        let registry = registry_with(now);
        assert_eq!(
            registry.claim("instance", "/p", "ask", "hash", std::process::id()),
            Some("session".into())
        );
        assert!(registry
            .claim("instance", "/p", "ask", "hash", std::process::id())
            .is_none());
    }

    #[test]
    fn grok_registration_and_pending_partition_require_unique_live_process_project() {
        let registry = GrokBindingRegistry::default();
        let pid = std::process::id();
        let first = uuid::Uuid::new_v4().to_string();
        let second = uuid::Uuid::new_v4().to_string();
        registry.register_resolved("not-a-uuid".into(), "/p".into(), pid, 42);
        registry.register_resolved(first.clone(), String::new(), pid, 42);
        assert!(registry.inner.lock().unwrap().instances.is_empty());

        registry.register_resolved(first.clone(), "/p".into(), pid, 42);
        registry.add_pending_resolved(
            "session-1".into(),
            "askhuman__show_last".into(),
            "hash".into(),
            "/p".into(),
            42,
            Some("tool-1".into()),
            crate::history::now_ms(),
        );
        assert_eq!(
            registry.claim(&first, "/p", "show_last", "hash", pid),
            Some("session-1".into())
        );

        registry.register_resolved(second.clone(), "/p".into(), pid, 42);
        registry.add_pending_resolved(
            "ambiguous".into(),
            "askhuman__show_last".into(),
            "same".into(),
            "/p".into(),
            42,
            None,
            crate::history::now_ms(),
        );
        assert!(registry.inner.lock().unwrap().pending.is_empty());

        registry.add_pending_resolved(
            "different-process".into(),
            "askhuman__show_last".into(),
            "hash".into(),
            "/p".into(),
            99,
            None,
            crate::history::now_ms(),
        );
        registry.add_pending_resolved(
            "different-project".into(),
            "askhuman__show_last".into(),
            "hash".into(),
            "/other".into(),
            42,
            None,
            crate::history::now_ms(),
        );
        registry.add_pending_resolved(
            "".into(),
            "askhuman__other".into(),
            "hash".into(),
            "/p".into(),
            42,
            None,
            crate::history::now_ms(),
        );
        assert!(registry.inner.lock().unwrap().pending.is_empty());
    }
}
