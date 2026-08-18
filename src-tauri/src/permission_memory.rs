//! Hook-side Codex permission memory analysis (spec `docs/specs/codex-permission-remember.md`).
//!
//! Runs inside the short-lived `__permission-hook` process. Given the raw PermissionRequest
//! stdin, it derives which memory choices the current request provably supports and the
//! matching auto-allow query. Everything here is a fallible enhancement on top of the basic
//! allow-once/deny confirmation (D24): any parse failure, rollout gap or guardian ambiguity
//! yields `Analysis::Basic` and keeps the existing popup unchanged.

use crate::confirm::ActionRole;
use crate::models::ConfirmChoice;
use crate::permission_rules::{
    MemoryQuery, MemorySave, NativeWrite, PermissionMemory, RuleKey, RuleNamespace,
};
use serde_json::Value;
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// Choice action ids added by the memory layer. The daemon validates them against saves.
pub const ACTION_REMEMBER_FILES: &str = "remember_files";
pub const ACTION_REMEMBER_PROJECT: &str = "remember_project";
pub const ACTION_REMEMBER_DISK: &str = "remember_disk";
pub const ACTION_MCP_SESSION: &str = "remember_mcp_session";
pub const ACTION_MCP_ALWAYS: &str = "remember_mcp_always";
pub const ACTION_NETWORK_SESSION: &str = "remember_network_session";
pub const ACTION_NETWORK_ALWAYS: &str = "remember_network_always";
pub const ACTION_SHELL_SESSION: &str = "remember_shell_session";
pub const ACTION_SHELL_PREFIX: &str = "remember_shell_prefix";
pub const ACTION_SHELL_ALWAYS: &str = "remember_shell_always";
pub const ACTION_SHELL_RELAXED: &str = "remember_shell_relaxed";
pub const ACTION_YOLO: &str = "remember_yolo";

/// Outcome of the hook-side analysis.
pub enum Analysis {
    /// Provably guardian-routed (D36): the hook must exit without any decision or popup.
    Suppress,
    /// No memory enhancement for this request; show the basic allow-once/deny popup.
    Basic,
    /// Every shell segment is explicitly allowed by the latest native policy (D42): the
    /// hook answers allow without any surface.
    AutoAllow,
    /// Memory options and auto-allow query for this request.
    Enhanced {
        memory: PermissionMemory,
        /// Choices inserted between allow-once and deny (§4.3 order).
        extra_choices: Vec<ConfirmChoice>,
    },
}

/// Runs the isolated shell analysis (production: `permission_shell::analyze_for_hook`).
/// Injected so unit tests never spawn workers or depend on an installed Codex.
pub type ShellRunner<'a> = &'a dyn Fn(
    &crate::permission_shell::ShellProbe,
) -> Option<crate::permission_shell::ShellWorkerOutput>;

/// Analyze a Codex PermissionRequest for memory support. `input` is the full hook stdin.
pub fn analyze_codex(input: &Value, zh: bool) -> Analysis {
    analyze_codex_with(input, zh, &crate::permission_shell::analyze_for_hook)
}

pub fn analyze_codex_with(input: &Value, zh: bool, shell_runner: ShellRunner<'_>) -> Analysis {
    let object = match input.as_object() {
        Some(object) => object,
        None => return Analysis::Basic,
    };
    let tool_name = object
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let tool_input = object.get("tool_input").cloned().unwrap_or(Value::Null);
    let cwd = object.get("cwd").and_then(Value::as_str).unwrap_or("");

    // Per-type derivation first (cheap), rollout gate second (I/O).
    enum Pending {
        Ready((PermissionMemory, Vec<ConfirmChoice>)),
        /// Built-in AskHuman self-call whitelist (D49): auto-allow once the gate passes.
        SelfCall,
        /// Network candidate: needs the rollout owner-command cross-check (D39 condition 3).
        Network(NetworkTarget),
        /// Plain shell script: needs the owner FunctionCall fields + worker (D38/D44).
        Shell(String),
    }
    let pending = if tool_name == "apply_patch" {
        // Verifiable native structured file edits (D13/D46).
        patch_paths(&tool_input, cwd)
            .map(|paths| file_edit_enhancement(&paths, cwd, zh))
            .map(Pending::Ready)
    } else if tool_name.starts_with("mcp__") {
        // MCP tool call: tool_name is registry-generated, not model-forgeable (D40).
        if mcp_self_call(tool_name, cwd) {
            Some(Pending::SelfCall)
        } else {
            mcp_enhancement(tool_name, cwd, zh).map(Pending::Ready)
        }
    } else if tool_name == "Bash" {
        // Network interception candidate (D39 conditions 1+2) first; anything else with a
        // command is a plain shell request (Phase 4).
        match network_target(&tool_input) {
            Some(target) => Some(Pending::Network(target)),
            None => tool_input
                .get("command")
                .and_then(Value::as_str)
                .map(|script| Pending::Shell(script.to_string())),
        }
    } else {
        None
    };
    let Some(pending) = pending else {
        return Analysis::Basic;
    };

    // Guardian gate (D36, D43 relaxed per D50): memory options and auto-allow require a
    // successful rollout read proving the reviewer is the user. Upstream (#32232, in
    // 0.145.0) now runs permission hooks before the automated reviewer and treats a hook
    // decision as final even under turn-level strict auto-review, so request_permissions
    // activity no longer disables memory. A proven guardian route suppresses the popup
    // entirely; anything unprovable keeps the basic popup.
    let owner_command = match &pending {
        Pending::Network(target) => Some(target.command.as_str()),
        Pending::Shell(script) => Some(script.as_str()),
        Pending::Ready(_) | Pending::SelfCall => None,
    };
    let Some(scan) = rollout_scan(object, owner_command) else {
        return Analysis::Basic;
    };
    match evaluate_gate(&scan) {
        RolloutGate::GuardianProven => return Analysis::Suppress,
        RolloutGate::MemoryAllowed => {}
        RolloutGate::Unproven => return Analysis::Basic,
    }

    let (memory, extra_choices) = match pending {
        Pending::Ready(enhancement) => enhancement,
        Pending::SelfCall => return Analysis::AutoAllow,
        Pending::Network(target) => {
            // D39 condition 3: the owner FunctionCall's model-written justification must
            // differ from the Codex-generated description, else this is a forged/plain
            // shell request and gets no network treatment.
            if !network_cross_check(&scan, &target) {
                return Analysis::Basic;
            }
            match network_enhancement(&target, zh) {
                Some(enhancement) => enhancement,
                None => return Analysis::Basic,
            }
        }
        Pending::Shell(script) => {
            // Built-in self-call whitelist (D49): a lone literal `AskHuman` ask-style
            // invocation resolving to this very binary never needs a popup.
            if shell_self_call(&script, cwd) {
                return Analysis::AutoAllow;
            }
            match shell_enhancement(&scan, &script, cwd, zh, shell_runner) {
                ShellOutcome::AutoAllow => return Analysis::AutoAllow,
                ShellOutcome::Enhanced(enhancement) => enhancement,
                ShellOutcome::Basic => return Analysis::Basic,
            }
        }
    };
    Analysis::Enhanced {
        memory,
        extra_choices,
    }
}

/// All old/new paths of the patch, lexically normalized against the hook cwd (D46).
/// `None` when the payload is not a fully parseable apply_patch envelope.
fn patch_paths(tool_input: &Value, cwd: &str) -> Option<Vec<String>> {
    let command = tool_input.get("command")?.as_str()?;
    let files = crate::permission_diff::patch::parse_apply_patch(
        command,
        crate::permission_diff::MAX_FILES,
    )
    .ok()?;
    let mut paths: Vec<String> = Vec::new();
    for file in &files {
        let mut push = |raw: &str| -> Option<()> {
            let normalized = crate::permission_rules::normalize_path(raw, cwd)?;
            if !paths.contains(&normalized) {
                paths.push(normalized);
            }
            Some(())
        };
        if let Some(old_path) = file.old_path.as_deref() {
            push(old_path)?;
        }
        push(&file.new_path)?;
    }
    (!paths.is_empty()).then_some(paths)
}

fn file_edit_enhancement(
    paths: &[String],
    cwd: &str,
    zh: bool,
) -> (PermissionMemory, Vec<ConfirmChoice>) {
    let session_note = if zh {
        "本对话（含 Resume 与本对话的子代理）"
    } else {
        "This conversation (including resume and its sub-agents)"
    };
    let mut extra_choices = Vec::new();
    let mut saves = Vec::new();

    let count = paths.len();
    extra_choices.push(ConfirmChoice {
        id: ACTION_REMEMBER_FILES.into(),
        label: if zh {
            format!("本对话不再询问这些文件（{count} 个）")
        } else {
            format!("Don't ask again for these files in this conversation ({count})")
        },
        description: session_note.to_string(),
        role: ActionRole::Default,
        variant: None,
    });
    saves.push(MemorySave {
        action_id: ACTION_REMEMBER_FILES.into(),
        namespace: RuleNamespace::Session,
        rules: paths
            .iter()
            .map(|path| RuleKey::FileExact { path: path.clone() })
            .collect(),
        native: None,
    });

    // Aggregated scope (D10/D11): project root from the hook cwd (D12).
    let project_root = crate::permission_rules::normalize_path(".", cwd).map(|normalized_cwd| {
        crate::project::git_root(std::path::Path::new(&normalized_cwd))
            .map(|root| root.to_string_lossy().to_string())
            .unwrap_or(normalized_cwd)
    });
    match project_root {
        Some(root) if paths.iter().all(|path| within_root(path, &root)) => {
            extra_choices.push(ConfirmChoice {
                id: ACTION_REMEMBER_PROJECT.into(),
                label: if zh {
                    "本对话允许所有本项目内的文件修改".into()
                } else {
                    "Allow all file changes inside this project for this conversation".into()
                },
                description: format!("{session_note} · {root}"),
                role: ActionRole::Default,
                variant: None,
            });
            saves.push(MemorySave {
                action_id: ACTION_REMEMBER_PROJECT.into(),
                namespace: RuleNamespace::Session,
                rules: vec![RuleKey::FileProject { root }],
                native: None,
            });
        }
        Some(_) => {
            extra_choices.push(ConfirmChoice {
                id: ACTION_REMEMBER_DISK.into(),
                label: if zh {
                    "本对话允许完全磁盘文件修改".into()
                } else {
                    "Allow file changes anywhere on disk for this conversation".into()
                },
                // Danger styling comes from the Destructive role (§4.3).
                description: session_note.to_string(),
                role: ActionRole::Destructive,
                variant: None,
            });
            saves.push(MemorySave {
                action_id: ACTION_REMEMBER_DISK.into(),
                namespace: RuleNamespace::Session,
                rules: vec![RuleKey::FileDisk],
                native: None,
            });
        }
        // Unresolvable cwd: exact-file option only (§4.2).
        None => {}
    }

    (
        PermissionMemory {
            query: Some(MemoryQuery::FileEdit {
                paths: paths.to_vec(),
            }),
            saves,
        },
        extra_choices,
    )
}

fn within_root(path: &str, root: &str) -> bool {
    crate::permission_rules::path_within_text(path, root)
}

// ===== MCP tools (D40/D41, §6.5.2) =====

/// Where the permanent "always allow" of an MCP tool lands.
enum McpPermanent {
    /// Server defined in a writable native layer: `approval_mode = "approve"` edit.
    Native {
        config_path: PathBuf,
        server: String,
        tool: String,
    },
    /// No native channel we can prove (plugin / codex_apps / ambiguous): D41 global shadow.
    Shadow,
    /// A readable layer explicitly sets prompt/writes for this tool: no memory options.
    RespectExplicitPrompt,
}

fn mcp_enhancement(
    tool_name: &str,
    cwd: &str,
    zh: bool,
) -> Option<(PermissionMemory, Vec<ConfirmChoice>)> {
    mcp_enhancement_at(tool_name, cwd, zh, &default_codex_home()?)
}

pub(crate) fn default_codex_home() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("CODEX_HOME").map(PathBuf::from) {
        if home.is_absolute() {
            return Some(home);
        }
    }
    dirs::home_dir().map(|home| home.join(".codex"))
}

fn mcp_enhancement_at(
    tool_name: &str,
    cwd: &str,
    zh: bool,
    codex_home: &Path,
) -> Option<(PermissionMemory, Vec<ConfirmChoice>)> {
    // Structural sanity; the full string is also the session rule key (D40).
    if !tool_name.starts_with("mcp__") || tool_name.len() <= 5 || tool_name.len() > 1_024 {
        return None;
    }

    let permanent = mcp_permanent_channel(tool_name, cwd, codex_home);
    if matches!(permanent, McpPermanent::RespectExplicitPrompt) {
        // User explicitly configured prompting for this tool: like native, no memory offer.
        return None;
    }

    let session_note = if zh {
        "本对话（含 Resume 与本对话的子代理）"
    } else {
        "This conversation (including resume and its sub-agents)"
    };
    let mut extra_choices = vec![ConfirmChoice {
        id: ACTION_MCP_SESSION.into(),
        label: if zh {
            "本对话允许此工具".into()
        } else {
            "Allow this tool for this conversation".into()
        },
        description: session_note.to_string(),
        role: ActionRole::Default,
        variant: None,
    }];
    let mut saves = vec![MemorySave {
        action_id: ACTION_MCP_SESSION.into(),
        namespace: RuleNamespace::Session,
        rules: vec![RuleKey::McpTool {
            tool: tool_name.to_string(),
        }],
        native: None,
    }];

    match permanent {
        McpPermanent::Native {
            config_path,
            server,
            tool,
        } => {
            extra_choices.push(ConfirmChoice {
                id: ACTION_MCP_ALWAYS.into(),
                label: if zh {
                    "始终允许此工具（写入 Codex 配置）".into()
                } else {
                    "Always allow this tool (saved to Codex config)".into()
                },
                description: config_path.to_string_lossy().to_string(),
                role: ActionRole::Default,
                variant: None,
            });
            saves.push(MemorySave {
                action_id: ACTION_MCP_ALWAYS.into(),
                // Session bridge rule: the running Codex does not reload our external
                // config write, so the current conversation is covered by shadow (D40).
                namespace: RuleNamespace::Session,
                rules: vec![RuleKey::McpTool {
                    tool: tool_name.to_string(),
                }],
                native: Some(NativeWrite::McpApprovalMode {
                    config_path: config_path.to_string_lossy().to_string(),
                    server,
                    tool,
                }),
            });
        }
        McpPermanent::Shadow => {
            extra_choices.push(ConfirmChoice {
                id: ACTION_MCP_ALWAYS.into(),
                label: if zh {
                    "始终允许此工具（由 AskHuman 记住）".into()
                } else {
                    "Always allow this tool (remembered by AskHuman)".into()
                },
                description: if zh {
                    "跨会话生效，30 天未使用自动过期".into()
                } else {
                    "Applies across conversations; expires after 30 days unused".into()
                },
                role: ActionRole::Default,
                variant: None,
            });
            saves.push(MemorySave {
                action_id: ACTION_MCP_ALWAYS.into(),
                namespace: RuleNamespace::Global,
                rules: vec![RuleKey::McpTool {
                    tool: tool_name.to_string(),
                }],
                native: None,
            });
        }
        McpPermanent::RespectExplicitPrompt => unreachable!(),
    }

    Some((
        PermissionMemory {
            query: Some(MemoryQuery::McpTool {
                tool: tool_name.to_string(),
            }),
            saves,
        },
        extra_choices,
    ))
}

/// One readable Codex config layer, highest precedence first (project nearest → user).
struct ConfigLayer {
    path: PathBuf,
    doc: toml_edit::DocumentMut,
    /// Directory containing `.codex` for project layers, `None` for the user layer.
    project_root: Option<PathBuf>,
}

/// Readable layers: `.codex/config.toml` from the hook cwd up to the git root (nearest
/// first), then `$CODEX_HOME/config.toml`. Unparseable files are skipped (fail toward
/// the shadow fallback, never toward a native write).
fn readable_config_layers(cwd: &str, codex_home: &Path) -> Vec<ConfigLayer> {
    let mut layers = Vec::new();
    if let Some(normalized_cwd) = crate::permission_rules::normalize_path(".", cwd) {
        let cwd_path = PathBuf::from(&normalized_cwd);
        let stop = crate::project::git_root(&cwd_path).unwrap_or_else(|| cwd_path.clone());
        let mut dir = Some(cwd_path.as_path());
        while let Some(current) = dir {
            if let Some(layer) = load_layer(&current.join(".codex/config.toml")) {
                layers.push(ConfigLayer {
                    project_root: Some(current.to_path_buf()),
                    ..layer
                });
            }
            if current == stop {
                break;
            }
            dir = current.parent();
        }
    }
    if let Some(layer) = load_layer(&codex_home.join("config.toml")) {
        layers.push(layer);
    }
    layers
}

fn load_layer(path: &Path) -> Option<ConfigLayer> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc = text.parse::<toml_edit::DocumentMut>().ok()?;
    Some(ConfigLayer {
        path: path.to_path_buf(),
        doc,
        project_root: None,
    })
}

/// Server names defined under `[mcp_servers]` of one layer.
fn layer_server_names(layer: &ConfigLayer) -> Vec<String> {
    layer
        .doc
        .as_table()
        .get("mcp_servers")
        .and_then(toml_edit::Item::as_table_like)
        .map(|table| table.iter().map(|(key, _)| key.to_string()).collect())
        .unwrap_or_default()
}

/// Explicit approval mode for `server`/`tool` in one layer: tool-level `approval_mode`,
/// falling back to the server's `default_tools_approval_mode`.
fn layer_explicit_mode(layer: &ConfigLayer, server: &str, tool: &str) -> Option<String> {
    let server_table = layer
        .doc
        .as_table()
        .get("mcp_servers")
        .and_then(toml_edit::Item::as_table_like)?
        .get(server)
        .and_then(toml_edit::Item::as_table_like)?;
    let tool_mode = server_table
        .get("tools")
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|tools| tools.get(tool))
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|entry| entry.get("approval_mode"))
        .and_then(toml_edit::Item::as_str);
    tool_mode
        .or_else(|| {
            server_table
                .get("default_tools_approval_mode")
                .and_then(toml_edit::Item::as_str)
        })
        .map(str::to_string)
}

/// Trust check against the user config on disk, for callers outside this module
/// (shell worker rules-layer discovery, D33).
pub(crate) fn codex_project_trusted(codex_home: &Path, root: &Path) -> bool {
    let user_layer = load_layer(&codex_home.join("config.toml"));
    project_is_trusted(user_layer.as_ref(), root)
}

/// D33-lite trust check: `[projects."<root>"] trust_level = "trusted"` in the user
/// config, matching either the raw root string or its canonicalized form. Anything
/// unprovable is untrusted (fail toward shadow).
fn project_is_trusted(user_layer: Option<&ConfigLayer>, root: &Path) -> bool {
    let Some(user_layer) = user_layer else {
        return false;
    };
    let Some(projects) = user_layer
        .doc
        .as_table()
        .get("projects")
        .and_then(toml_edit::Item::as_table_like)
    else {
        return false;
    };
    let mut keys = vec![root.to_string_lossy().to_string()];
    if let Ok(canonical) = std::fs::canonicalize(root) {
        let canonical = canonical.to_string_lossy().to_string();
        if !keys.contains(&canonical) {
            keys.push(canonical);
        }
    }
    keys.iter().any(|key| {
        projects
            .get(key)
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|entry| entry.get("trust_level"))
            .and_then(toml_edit::Item::as_str)
            == Some("trusted")
    })
}

fn mcp_permanent_channel(tool_name: &str, cwd: &str, codex_home: &Path) -> McpPermanent {
    // codex_apps connector tools need a connector_id we cannot derive: D41 shadow.
    if tool_name.starts_with("mcp__codex_apps__") {
        return McpPermanent::Shadow;
    }
    let layers = readable_config_layers(cwd, codex_home);
    let user_layer = layers.iter().find(|layer| layer.project_root.is_none());

    // Candidate (layer index, server, bare tool) whose name splits the hook tool_name.
    let mut candidates: Vec<(usize, String, String)> = Vec::new();
    for (index, layer) in layers.iter().enumerate() {
        for server in layer_server_names(layer) {
            let prefix = format!("mcp__{server}__");
            if let Some(tool) = tool_name.strip_prefix(&prefix) {
                if !tool.is_empty() {
                    candidates.push((index, server, tool.to_string()));
                }
            }
        }
    }

    // Respect explicit prompt/writes config on any candidate reading (D40): a higher layer
    // could override in the effective merge, but hiding the option is the safe direction.
    for (index, server, tool) in &candidates {
        if let Some(mode) = layer_explicit_mode(&layers[*index], server, tool) {
            if mode == "prompt" || mode == "writes" {
                return McpPermanent::RespectExplicitPrompt;
            }
        }
    }

    // An unambiguous server name is required for a native write (`__` split ambiguity).
    let mut names: Vec<&str> = candidates
        .iter()
        .map(|(_, server, _)| server.as_str())
        .collect();
    names.sort_unstable();
    names.dedup();
    if names.len() != 1 {
        return McpPermanent::Shadow;
    }

    // Codex persists to the defining project layer first, then the user layer.
    for (index, server, tool) in &candidates {
        let layer = &layers[*index];
        match &layer.project_root {
            Some(root) => {
                if project_is_trusted(user_layer, root) {
                    return McpPermanent::Native {
                        config_path: layer.path.clone(),
                        server: server.clone(),
                        tool: tool.clone(),
                    };
                }
                // Untrusted project layer is never a write target; keep looking lower.
            }
            None => {
                return McpPermanent::Native {
                    config_path: layer.path.clone(),
                    server: server.clone(),
                    tool: tool.clone(),
                };
            }
        }
    }
    McpPermanent::Shadow
}

// ===== Built-in AskHuman self-call whitelist (D49) =====

/// Flags a whitelisted ask-style invocation may use (each takes a following value,
/// except `--stdin`). Any other `-`-prefixed token fails toward the popup.
const SELF_CALL_ASK_FLAGS: &[&str] = &[
    "-q",
    "--question",
    "-o",
    "--option",
    "-o!",
    "--option!",
    "-f",
    "--file",
    "--stdin",
];
/// `--whats-next` additionally rejects `-q` (its question is fixed).
const SELF_CALL_WHATS_NEXT_FLAGS: &[&str] = &[
    "-o",
    "--option",
    "-o!",
    "--option!",
    "-f",
    "--file",
    "--stdin",
];

/// Ask-style usages only (D49): free-form ask, `--whats-next`, `--agent-help`,
/// `--show-last`, `todo add`. Config / daemon / dev subcommands never ride the whitelist.
fn self_call_usage_allowed(args: &[String]) -> bool {
    fn flags_ok(args: &[String], allowed: &[&str]) -> bool {
        args.iter()
            .all(|arg| !arg.starts_with('-') || allowed.contains(&arg.as_str()))
    }
    match args.first().map(String::as_str) {
        None => false,
        // Read-only recovery of the last exchange; the interaction protocol requires agents to run
        // it after a compaction, so making them ask for approval first is pure friction.
        Some("--agent-help") => args.len() == 1,
        // Read-only recovery; optional count 1..=10 after the flag (see show-last-multi).
        Some("--show-last") => {
            args.len() == 1
                || (args.len() == 2 && args[1].parse::<u32>().is_ok_and(|n| (1..=10).contains(&n)))
        }
        Some("todo") => {
            args.get(1).map(String::as_str) == Some("add")
                && args.len() >= 3
                && args[2..].iter().all(|arg| !arg.starts_with('-'))
        }
        Some("--whats-next") => flags_ok(&args[1..], SELF_CALL_WHATS_NEXT_FLAGS),
        Some(first) => {
            // A leading positional is the ask message. A bare command-like token
            // ("daemon", "config", "__x"...) could instead dispatch a CLI subcommand
            // — current or future — so it fails toward the popup; real messages
            // contain spaces, punctuation, or non-ASCII.
            let command_like = !first.starts_with('-')
                && first
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            !command_like && flags_ok(args, SELF_CALL_ASK_FLAGS)
        }
    }
}

/// Shell whitelist: the script is a single literal `AskHuman` invocation (per
/// `self_call_usage_allowed`) whose argv[0] provably resolves to this very binary.
fn shell_self_call(script: &str, cwd: &str) -> bool {
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    shell_self_call_at(script, cwd, &current, std::env::var_os("PATH").as_deref())
}

fn shell_self_call_at(
    script: &str,
    cwd: &str,
    current_exe: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> bool {
    let Some(argv) = crate::shell_safety::parse_lone_literal_command(script) else {
        return false;
    };
    let Some((program, args)) = argv.split_first() else {
        return false;
    };
    if !self_call_usage_allowed(args) {
        return false;
    }
    let Ok(current) = std::fs::canonicalize(current_exe) else {
        return false;
    };
    let candidate = if program.contains(['/', '\\']) {
        if crate::permission_rules::path_is_absolute_text(program) {
            PathBuf::from(program)
        } else {
            let Some(base) = crate::permission_rules::normalize_path(".", cwd) else {
                return false;
            };
            Path::new(&base).join(program)
        }
    } else {
        // PATH lookup mirroring exec semantics; relative PATH entries never count
        // (a workspace-planted binary must not ride the whitelist).
        let Some(found) = find_in_path(program, path_var) else {
            return false;
        };
        found
    };
    // Canonicalized (symlink-resolved) identity with the running hook binary.
    std::fs::canonicalize(&candidate)
        .map(|path| path == current)
        .unwrap_or(false)
}

fn find_in_path(name: &str, path_var: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    for dir in std::env::split_paths(path_var) {
        if !dir.is_absolute() {
            continue;
        }
        let candidate = dir.join(name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// MCP whitelist: `mcp__askhuman__{ask,whats_next,todo_add}` where every readable config layer
/// defining an `askhuman` server points its `command` at this binary, and none sets an
/// explicit prompt/writes approval mode for the tool (user intent wins, mirrors D40).
fn mcp_self_call(tool_name: &str, cwd: &str) -> bool {
    let Some(codex_home) = default_codex_home() else {
        return false;
    };
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    mcp_self_call_at(tool_name, cwd, &codex_home, &current)
}

fn mcp_self_call_at(tool_name: &str, cwd: &str, codex_home: &Path, current_exe: &Path) -> bool {
    let tool = match tool_name {
        "mcp__askhuman__ask" => "ask",
        "mcp__askhuman__whats_next" => "whats_next",
        "mcp__askhuman__todo_add" => "todo_add",
        _ => return false,
    };
    let Ok(current) = std::fs::canonicalize(current_exe) else {
        return false;
    };
    let layers = readable_config_layers(cwd, codex_home);
    let mut defined = false;
    for layer in &layers {
        let Some(server) = layer
            .doc
            .as_table()
            .get("mcp_servers")
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|table| table.get("askhuman"))
            .and_then(toml_edit::Item::as_table_like)
        else {
            continue;
        };
        defined = true;
        if let Some(mode) = layer_explicit_mode(layer, "askhuman", tool) {
            if mode == "prompt" || mode == "writes" {
                return false;
            }
        }
        // A workspace-defined `askhuman` server pointing anywhere else must not ride
        // the whitelist; only absolute commands are resolvable.
        let Some(command) = server.get("command").and_then(toml_edit::Item::as_str) else {
            return false;
        };
        if !crate::permission_rules::path_is_absolute_text(command) {
            return false;
        }
        match std::fs::canonicalize(command) {
            Ok(path) if path == current => {}
            _ => return false,
        }
    }
    defined
}

// ===== Claude Code self-call whitelist (D49) =====

/// D49 for Claude Code: auto-allow AskHuman's own shell / MCP invocations. Claude has no
/// guardian/rollout gate — its PermissionRequest hook only fires once Claude itself decided
/// to ask the user — so the whitelist applies directly, gated on explicit user rules.
pub fn claude_self_call(input: &Value) -> bool {
    let Some(home) = claude_home() else {
        return false;
    };
    let Ok(current) = std::env::current_exe() else {
        return false;
    };
    claude_self_call_at(input, &home, &current, std::env::var_os("PATH").as_deref())
}

fn claude_home() -> Option<PathBuf> {
    dirs::home_dir()
}

pub(crate) fn claude_self_call_at(
    input: &Value,
    home: &Path,
    current_exe: &Path,
    path_var: Option<&std::ffi::OsStr>,
) -> bool {
    let Some(object) = input.as_object() else {
        return false;
    };
    let tool_name = object
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("");
    let cwd = object.get("cwd").and_then(Value::as_str).unwrap_or("");
    if !crate::permission_rules::path_is_absolute_text(cwd) {
        return false;
    }
    match tool_name {
        "Bash" => {
            let Some(script) = object
                .get("tool_input")
                .and_then(|tool_input| tool_input.get("command"))
                .and_then(Value::as_str)
            else {
                return false;
            };
            !claude_explicit_rule_conflict(cwd, home, true)
                && shell_self_call_at(script, cwd, current_exe, path_var)
        }
        "mcp__askhuman__ask" | "mcp__askhuman__whats_next" | "mcp__askhuman__todo_add" => {
            !claude_explicit_rule_conflict(cwd, home, false)
                && claude_mcp_self_call_at(cwd, home, current_exe)
        }
        _ => false,
    }
}

/// An explicit `permissions.ask` / `permissions.deny` rule mentioning AskHuman in any
/// readable Claude settings layer wins over the whitelist (mirrors D40's respect for
/// explicit prompt modes). Matching is deliberately substring-coarse: a false hit only
/// costs a popup.
fn claude_explicit_rule_conflict(cwd: &str, home: &Path, shell: bool) -> bool {
    for path in claude_settings_paths(cwd, home) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            // An unparseable settings file could hide an explicit rule: fail to popup.
            return true;
        };
        for list in ["ask", "deny"] {
            let rules = value
                .get("permissions")
                .and_then(|permissions| permissions.get(list))
                .and_then(Value::as_array);
            for rule in rules.into_iter().flatten() {
                let Some(rule) = rule.as_str() else { continue };
                let lower = rule.to_ascii_lowercase();
                let hit = if shell {
                    lower.starts_with("bash") && lower.contains("askhuman")
                } else {
                    lower.starts_with("mcp__askhuman")
                };
                if hit {
                    return true;
                }
            }
        }
    }
    false
}

/// Claude settings layers carrying permission rules: enterprise managed, user, then
/// project `settings.json` / `settings.local.json` from the hook cwd up to the git root.
fn claude_settings_paths(cwd: &str, home: &Path) -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json"),
        PathBuf::from("/etc/claude-code/managed-settings.json"),
        home.join(".claude/settings.json"),
    ];
    for dir in claude_walk_up_dirs(cwd) {
        paths.push(dir.join(".claude/settings.json"));
        paths.push(dir.join(".claude/settings.local.json"));
    }
    paths
}

/// Directories from the normalized cwd up to the git root (or just the cwd without one),
/// mirroring `readable_config_layers`.
fn claude_walk_up_dirs(cwd: &str) -> Vec<PathBuf> {
    let Some(normalized) = crate::permission_rules::normalize_path(".", cwd) else {
        return Vec::new();
    };
    let cwd_path = PathBuf::from(&normalized);
    let stop = crate::project::git_root(&cwd_path).unwrap_or_else(|| cwd_path.clone());
    let mut dirs = Vec::new();
    let mut dir = Some(cwd_path.as_path());
    while let Some(current) = dir {
        dirs.push(current.to_path_buf());
        if current == stop {
            break;
        }
        dir = current.parent();
    }
    dirs
}

/// Every readable Claude layer defining an `askhuman` MCP server must point its `command`
/// at this binary (user `~/.claude.json` top-level + matching `projects` entries, and
/// project `.mcp.json` files); a workspace-planted impostor keeps the popup.
fn claude_mcp_self_call_at(cwd: &str, home: &Path, current_exe: &Path) -> bool {
    let Ok(current) = std::fs::canonicalize(current_exe) else {
        return false;
    };
    let server_is_us = |server: &Value| -> bool {
        let Some(command) = server.get("command").and_then(Value::as_str) else {
            return false;
        };
        if !crate::permission_rules::path_is_absolute_text(command) {
            return false;
        }
        matches!(std::fs::canonicalize(command), Ok(path) if path == current)
    };
    let mut defined = false;

    let user_path = home.join(".claude.json");
    if let Ok(text) = std::fs::read_to_string(&user_path) {
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        if let Some(server) = value
            .get("mcpServers")
            .and_then(|servers| servers.get("askhuman"))
        {
            defined = true;
            if !server_is_us(server) {
                return false;
            }
        }
        // Local-scope servers live under `projects["<launch dir>"]`; the launch dir is
        // the hook cwd or one of its ancestors.
        for (key, project) in value
            .get("projects")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let covers = within_root(cwd, key);
            if !covers {
                continue;
            }
            if let Some(server) = project
                .get("mcpServers")
                .and_then(|servers| servers.get("askhuman"))
            {
                defined = true;
                if !server_is_us(server) {
                    return false;
                }
            }
        }
    }

    for dir in claude_walk_up_dirs(cwd) {
        let Ok(text) = std::fs::read_to_string(dir.join(".mcp.json")) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        if let Some(server) = value
            .get("mcpServers")
            .and_then(|servers| servers.get("askhuman"))
        {
            defined = true;
            if !server_is_us(server) {
                return false;
            }
        }
    }
    defined
}

// ===== Network host approvals (D39, §6.5.1) =====

/// A Bash PermissionRequest that structurally looks like a Codex network interception
/// (D39 conditions 1+2). Condition 3 (rollout cross-check) is applied separately.
struct NetworkTarget {
    /// The triggering shell command (or `network-access {target}` when ownerless).
    command: String,
    /// The full hook `description` field, `network-access {proto}://{host}:{port}`.
    description: String,
    host: String,
    protocol: String,
    port: u16,
}

/// Strict machine-format parse of the hook `description`; anything that does not
/// round-trip byte-exactly is treated as a plain shell request.
fn network_target(tool_input: &Value) -> Option<NetworkTarget> {
    let command = tool_input.get("command")?.as_str()?.to_string();
    let description = tool_input.get("description")?.as_str()?.to_string();
    let target = description.strip_prefix("network-access ")?;
    let (protocol, rest) = target.split_once("://")?;
    if !crate::permission_rules::NETWORK_PROTOCOLS.contains(&protocol) {
        return None;
    }
    let (host, port_text) = rest.rsplit_once(':')?;
    if port_text.is_empty() || !port_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let port: u16 = port_text.parse().ok()?;
    if !crate::permission_rules::network_host_is_valid(host) {
        return None;
    }
    // Round-trip check pins the exact `format_network_target` shape (no extra text).
    if description != format!("network-access {protocol}://{host}:{port}") {
        return None;
    }
    let host = host.to_string();
    let protocol = protocol.to_string();
    Some(NetworkTarget {
        command,
        description,
        host,
        protocol,
        port,
    })
}

/// D39 condition 3 against the rollout scan: genuine network descriptions are generated by
/// Codex, while a plain shell request's description is the model's own `justification` on
/// the owner FunctionCall. Fail closed toward plain-shell treatment.
fn network_cross_check(scan: &RolloutScan, target: &NetworkTarget) -> bool {
    if scan.owner_calls.is_empty() {
        // No owner call: only the native ownerless prompt-command shape qualifies.
        let target_text = format!("{}://{}:{}", target.protocol, target.host, target.port);
        return target.command == format!("network-access {target_text}");
    }
    // D44 (refined): the forgery check is per call — a justification equal to the
    // Codex-generated description means the model authored the description itself.
    // Diverging justifications across retries of the same command are harmless.
    !scan
        .owner_calls
        .iter()
        .any(|call| call.justification.as_deref() == Some(target.description.as_str()))
}

fn network_enhancement(
    target: &NetworkTarget,
    zh: bool,
) -> Option<(PermissionMemory, Vec<ConfirmChoice>)> {
    let rules_path = default_codex_home()?.join("rules/default.rules");
    let host = &target.host;
    let session_note = if zh {
        "本对话（含 Resume 与本对话的子代理）"
    } else {
        "This conversation (including resume and its sub-agents)"
    };
    let key = RuleKey::NetworkHost {
        host: host.clone(),
        protocol: target.protocol.clone(),
        port: target.port,
    };
    let extra_choices = vec![
        ConfirmChoice {
            id: ACTION_NETWORK_SESSION.into(),
            label: if zh {
                format!("本对话允许访问 {host}")
            } else {
                format!("Allow access to {host} for this conversation")
            },
            description: format!(
                "{session_note} · {}://{host}:{}",
                target.protocol, target.port
            ),
            role: ActionRole::Default,
            variant: None,
        },
        ConfirmChoice {
            id: ACTION_NETWORK_ALWAYS.into(),
            label: if zh {
                format!("始终允许访问 {host}（写入 Codex 全局规则）")
            } else {
                format!("Always allow access to {host} (saved to Codex global rules)")
            },
            // The native network_rule is host+protocol wide (any port).
            description: format!("{}://{host}", target.protocol),
            role: ActionRole::Default,
            variant: None,
        },
    ];
    let saves = vec![
        MemorySave {
            action_id: ACTION_NETWORK_SESSION.into(),
            namespace: RuleNamespace::Session,
            rules: vec![key.clone()],
            native: None,
        },
        MemorySave {
            action_id: ACTION_NETWORK_ALWAYS.into(),
            // Session bridge: the running Codex never reloads default.rules (D39).
            namespace: RuleNamespace::Session,
            rules: vec![key.clone()],
            native: Some(NativeWrite::NetworkRule {
                rules_path: rules_path.to_string_lossy().to_string(),
                host: host.clone(),
                protocol: target.protocol.clone(),
            }),
        },
    ];
    Some((
        PermissionMemory {
            query: Some(MemoryQuery::NetworkHost {
                host: host.clone(),
                protocol: target.protocol.clone(),
                port: target.port,
            }),
            saves,
        },
        extra_choices,
    ))
}

// ===== Plain shell requests (D28–D35, D38, D42–D45, §6.1) =====

enum ShellOutcome {
    /// Every segment is explicitly allowed by the latest native policy (D42/D28).
    AutoAllow,
    Enhanced((PermissionMemory, Vec<ConfirmChoice>)),
    Basic,
}

/// Collapse the rollout turn context into the two fields the worker's heuristic
/// replication needs. `None` when the schema is not the verified shape (fail closed).
fn shell_turn_fields(context: &Value) -> Option<(String, String)> {
    let approval_policy = match context.get("approval_policy") {
        Some(Value::String(value)) => value.clone(),
        // AskForApproval::Granular is externally tagged: {"granular": {...}}.
        Some(Value::Object(map)) if map.contains_key("granular") => "granular".to_string(),
        _ => return None,
    };
    let sandbox_kind = match context
        .get("file_system_sandbox_policy")
        .and_then(|policy| policy.get("kind"))
        .and_then(Value::as_str)
    {
        Some(kind) => kind.to_string(),
        None => {
            // Legacy sandbox_policy fallback, mirroring from_legacy_sandbox_policy_for_cwd.
            let mode = context
                .get("sandbox_policy")
                .and_then(|policy| policy.get("type"))
                .and_then(Value::as_str)?;
            match mode {
                "danger-full-access" => "unrestricted".to_string(),
                "external-sandbox" => "external-sandbox".to_string(),
                "read-only" | "workspace-write" => "restricted".to_string(),
                _ => return None,
            }
        }
    };
    Some((approval_policy, sandbox_kind))
}

fn shell_enhancement(
    scan: &RolloutScan,
    script: &str,
    cwd: &str,
    zh: bool,
    shell_runner: ShellRunner<'_>,
) -> ShellOutcome {
    // D44 (refined 2026-07-18): owner calls supply prefix_rule / sandbox_permissions. No
    // owner call means we cannot prove a plain first-time shell approval (fail closed).
    // Multiple owner calls with diverging fields are a normal retry pattern (e.g. an
    // escalated attempt rejected under untrusted, then a plain retry), so fields merge
    // conservatively instead of abandoning the derivation.
    if scan.owner_calls.is_empty() {
        return ShellOutcome::Basic;
    }
    // Any escalated attempt makes the whole request escalated (more segments prompt).
    let sandbox_override = scan.owner_calls.iter().any(|call| {
        call.sandbox_permissions
            .as_deref()
            .is_some_and(|value| value != "use_default")
    });
    // The model prefix tier is only trusted when every owner call agrees on it;
    // otherwise the worker's fallback derivation still runs.
    let first_prefix = &scan.owner_calls[0].prefix_rule;
    let prefix_rule = if scan
        .owner_calls
        .iter()
        .all(|call| &call.prefix_rule == first_prefix)
    {
        first_prefix.clone()
    } else {
        None
    };
    let Some(context) = scan.turn_context.as_ref() else {
        return ShellOutcome::Basic;
    };
    let Some((approval_policy, sandbox_kind)) = shell_turn_fields(context) else {
        return ShellOutcome::Basic;
    };
    let probe = crate::permission_shell::ShellProbe {
        script: script.to_string(),
        cwd: cwd.to_string(),
        approval_policy,
        sandbox_kind,
        sandbox_override,
        prefix_rule,
    };
    let Some(output) = shell_runner(&probe) else {
        return ShellOutcome::Basic;
    };
    if output.disabled_reason.is_some() || output.segments.is_empty() {
        return ShellOutcome::Basic;
    }
    // D42/D28: every segment explicitly rule-allowed by the freshly re-read native policy
    // is Codex's own bypass_sandbox condition; a reloaded Codex would not prompt at all.
    if output.explicit_allow_all {
        return ShellOutcome::AutoAllow;
    }
    if output.decision == "forbidden" {
        // Our replication says native would forbid without prompting; something is off
        // with this request, so give it no memory surface.
        return ShellOutcome::Basic;
    }

    let session_note = if zh {
        "本对话（含 Resume 与本对话的子代理）"
    } else {
        "This conversation (including resume and its sub-agents)"
    };
    let mut extra_choices: Vec<ConfirmChoice> = Vec::new();
    let mut saves: Vec<MemorySave> = Vec::new();
    let mut query = None;

    // Relaxed-mode eligibility (D52): splittable (segments exist here), no native
    // prompt/forbidden hit, and no segment on either dangerous list. Rides on the
    // query so the daemon can honor an active relaxed grant without re-parsing.
    let relaxed_eligible = !output.policy_prompt_any
        && !output.dangerous_any
        && !output
            .segments
            .iter()
            .any(|segment| crate::shell_safety::is_relaxed_dangerous_command(segment));

    // Prefix generalization ladder (D51): worker-validated candidates, base amendment
    // first then progressively shorter truncations. Levels run shortest-first in the
    // selector; the smart 2-token recommendation keeps the unsuffixed action ids so IM
    // cards and history rendering stay on the recommended tier.
    let candidates = &output.amendment_candidates;
    let recommended = if candidates.is_empty() {
        0
    } else {
        recommended_candidate_index(candidates)
    };
    let ladder: Vec<(usize, &Vec<String>)> = candidates
        .iter()
        .enumerate()
        .rev() // shortest first
        .collect();
    let variant_for = |group: &str, index: usize, level: usize| {
        // Single-candidate ladders collapse to today's plain single choice.
        (candidates.len() > 1).then(|| crate::models::ChoiceVariant {
            group: group.to_string(),
            level,
            level_label: level_label(&candidates[index]),
            segment_label: segment_label(candidates, index),
            recommended: index == recommended,
        })
    };
    let action_id = |base: &str, index: usize| {
        if index == recommended {
            base.to_string()
        } else {
            format!("{base}:{index}")
        }
    };

    // Session tier and the auto-allow query are gated on the dangerous list (D38).
    // Prefix-first (D38 refined 2026-07-18): the prefix shares its source and validation
    // with the permanent amendment; the exact-commands tier only appears as a fallback
    // when no prefix could be derived.
    if !output.dangerous_any {
        if !candidates.is_empty() {
            for (level, (index, prefix)) in ladder.iter().enumerate() {
                let id = action_id(ACTION_SHELL_PREFIX, *index);
                let prefix_text = prefix.join(" ");
                extra_choices.push(ConfirmChoice {
                    id: id.clone(),
                    label: if zh {
                        format!("本对话允许 {prefix_text} 开头的命令")
                    } else {
                        format!("Allow commands starting with {prefix_text} this conversation")
                    },
                    description: session_note.to_string(),
                    role: ActionRole::Default,
                    variant: variant_for("shell-prefix-session", *index, level),
                });
                saves.push(MemorySave {
                    action_id: id,
                    namespace: RuleNamespace::Session,
                    rules: vec![RuleKey::ShellPrefix {
                        prefix: (*prefix).clone(),
                    }],
                    native: None,
                });
            }
        } else {
            let preview = shell_segments_preview(&output.segments);
            extra_choices.push(ConfirmChoice {
                id: ACTION_SHELL_SESSION.into(),
                label: if zh {
                    "本对话不再询问这些确切命令".into()
                } else {
                    "Don't ask again for these exact commands this conversation".into()
                },
                description: format!("{session_note} · {preview}"),
                role: ActionRole::Default,
                variant: None,
            });
            saves.push(MemorySave {
                action_id: ACTION_SHELL_SESSION.into(),
                namespace: RuleNamespace::Session,
                rules: output
                    .segments
                    .iter()
                    .map(|argv| RuleKey::ShellExact { argv: argv.clone() })
                    .collect(),
                native: None,
            });
        }
        query = Some(MemoryQuery::ShellCommands {
            commands: output.segments.clone(),
            relaxed_eligible,
        });
    }

    // Permanent tier mirrors the native TUI's conditional "don't ask again" option: shown
    // whenever the native amendment derivation proposes a prefix (D18 baseline). No
    // session bridge: default.rules is the single source of truth and is re-read on the
    // next request (D28).
    if !candidates.is_empty() {
        if let Some(home) = default_codex_home() {
            let rules_path = home.join("rules/default.rules");
            for (level, (index, prefix)) in ladder.iter().enumerate() {
                let id = action_id(ACTION_SHELL_ALWAYS, *index);
                let prefix_text = prefix.join(" ");
                extra_choices.push(ConfirmChoice {
                    id: id.clone(),
                    label: if zh {
                        format!("始终允许 {prefix_text} 开头的命令")
                    } else {
                        format!("Always allow commands starting with {prefix_text}")
                    },
                    description: prefix_text,
                    role: ActionRole::Default,
                    variant: variant_for("shell-prefix-always", *index, level),
                });
                saves.push(MemorySave {
                    action_id: id,
                    namespace: RuleNamespace::Session,
                    rules: Vec::new(),
                    native: Some(NativeWrite::PrefixRule {
                        rules_path: rules_path.to_string_lossy().to_string(),
                        prefix: (*prefix).clone(),
                    }),
                });
            }
        }
    }

    // Relaxed mode session opt-in (D52): offered on eligible (non-dangerous, splittable)
    // shell popups. Once saved, future eligible requests auto-allow daemon-side, so an
    // active grant never re-renders this choice. Destructive role = danger styling.
    if relaxed_eligible {
        extra_choices.push(ConfirmChoice {
            id: ACTION_SHELL_RELAXED.into(),
            label: if zh {
                "本对话开启宽松模式：只审危险命令".into()
            } else {
                "Relaxed mode this conversation: only audit dangerous commands".into()
            },
            description: if zh {
                format!("{session_note} · 非危险且可解析的 shell 命令自动放行")
            } else {
                format!("{session_note} · non-dangerous parseable shell commands auto-allow")
            },
            role: ActionRole::Destructive,
            variant: None,
        });
        saves.push(MemorySave {
            action_id: ACTION_SHELL_RELAXED.into(),
            namespace: RuleNamespace::Session,
            rules: vec![RuleKey::ShellRelaxed],
            native: None,
        });
    }

    if saves.is_empty() {
        return ShellOutcome::Basic;
    }
    ShellOutcome::Enhanced((PermissionMemory { query, saves }, extra_choices))
}

/// Smart recommended ladder entry (D51, Q3): command + subcommand when the second token
/// of the base amendment looks like a subcommand word; otherwise the bare command.
/// When that exact length was filtered out (banned/dangerous/uncovered), escalate to the
/// shortest longer candidate rather than a broader one, ultimately the base amendment.
/// `candidates` is base-first with strictly decreasing lengths and never empty.
fn recommended_candidate_index(candidates: &[Vec<String>]) -> usize {
    let base = &candidates[0];
    let desired = if base.len() >= 2 && subcommand_like(&base[1]) {
        2
    } else {
        1
    };
    let mut best = 0;
    for (index, candidate) in candidates.iter().enumerate() {
        if candidate.len() >= desired {
            best = index;
        }
    }
    best
}

/// A token that reads like a subcommand: not an option flag, not a path, not a number.
fn subcommand_like(token: &str) -> bool {
    !token.starts_with('-') && !token.contains('/') && token.parse::<f64>().is_err()
}

/// Short selector label for one ladder level.
fn level_label(prefix: &[String]) -> String {
    const MAX_LABEL: usize = 48;
    let text = prefix.join(" ");
    if text.chars().count() > MAX_LABEL {
        text.chars().take(MAX_LABEL).collect::<String>() + "…"
    } else {
        text
    }
}

/// Exact token chunk introduced by one shortest-first selector level. A skipped unsafe
/// intermediate prefix therefore becomes one indivisible, still-valid segment.
fn segment_label(candidates: &[Vec<String>], index: usize) -> String {
    let candidate = &candidates[index];
    let shorter_len = candidates
        .get(index + 1)
        .filter(|shorter| candidate.starts_with(shorter.as_slice()))
        .map_or(0, |shorter| shorter.len());
    candidate[shorter_len..].join(" ")
}

/// Short human preview of split segments for the exact-tier subtext.
fn shell_segments_preview(segments: &[Vec<String>]) -> String {
    const MAX_PREVIEW: usize = 120;
    let mut preview = segments
        .iter()
        .map(|argv| argv.join(" "))
        .collect::<Vec<_>>()
        .join(" && ");
    if preview.chars().count() > MAX_PREVIEW {
        preview = preview.chars().take(MAX_PREVIEW).collect::<String>() + "…";
    }
    preview
}

// ===== Rollout gate (D36/D43) =====

enum RolloutGate {
    /// TurnContextItem proves guardian routing: reviewer=auto_review + policy on-request/granular.
    GuardianProven,
    /// Reviewer is the user (turn-level strict auto-review no longer disables memory:
    /// upstream #32232 makes hook decisions authoritative before the automated reviewer).
    MemoryAllowed,
    /// Anything else: unreadable rollout, missing turn context, unknown schema.
    Unproven,
}

/// Upper bound on rollout size we are willing to scan (bail out beyond it: Unproven).
const MAX_ROLLOUT_BYTES: u64 = 512 * 1024 * 1024;
/// Upper bound on a single rollout line we parse.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Scan the transcript referenced by the hook input. `owner_command` optionally collects
/// the model-side `justification` of every current-turn FunctionCall whose command equals
/// it (D39/D44). `None` means the rollout is unreadable / unprovable.
fn rollout_scan(
    input: &serde_json::Map<String, Value>,
    owner_command: Option<&str>,
) -> Option<RolloutScan> {
    let transcript_path = input.get("transcript_path").and_then(Value::as_str)?;
    let turn_id = input
        .get("turn_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())?;
    scan_rollout(
        std::path::Path::new(transcript_path),
        turn_id,
        owner_command,
    )
}

/// Model-side fields of one current-turn FunctionCall matching the probed owner command.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OwnerCall {
    justification: Option<String>,
    prefix_rule: Option<Vec<String>>,
    /// Raw serde value of `sandbox_permissions` (`use_default` when absent).
    sandbox_permissions: Option<String>,
}

struct RolloutScan {
    turn_context: Option<Value>,
    /// Current-turn FunctionCalls matching the probed owner command (D39/D44).
    owner_calls: Vec<OwnerCall>,
}

fn scan_rollout(
    path: &std::path::Path,
    turn_id: &str,
    owner_command: Option<&str>,
) -> Option<RolloutScan> {
    let metadata = std::fs::metadata(path).ok()?;
    if metadata.len() > MAX_ROLLOUT_BYTES {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut scan = RolloutScan {
        turn_context: None,
        owner_calls: Vec::new(),
    };
    let mut buffer = Vec::new();
    let mut reader = reader;
    loop {
        buffer.clear();
        match read_capped_line(&mut reader, &mut buffer) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => return None,
        }
        let Ok(line) = std::str::from_utf8(&buffer) else {
            // Compressed or binary rollout: cannot prove anything.
            return None;
        };
        scan_rollout_line(line, turn_id, owner_command, &mut scan);
    }
    Some(scan)
}

/// Reads one line into `buffer` (without the trailing newline). Errors on oversized lines.
fn read_capped_line<R: BufRead>(reader: &mut R, buffer: &mut Vec<u8>) -> std::io::Result<usize> {
    use std::io::Read;
    let read = (&mut *reader)
        .take(MAX_LINE_BYTES as u64 + 1)
        .read_until(b'\n', buffer)?;
    if buffer.len() > MAX_LINE_BYTES {
        return Err(std::io::Error::other("rollout line too long"));
    }
    while buffer
        .last()
        .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
    {
        buffer.pop();
    }
    Ok(read)
}

fn scan_rollout_line(
    line: &str,
    turn_id: &str,
    owner_command: Option<&str>,
    scan: &mut RolloutScan,
) {
    // Cheap substring filters before any JSON parsing. When probing an owner command
    // every tool-call line must be parsed (its command may be JSON-escaped in raw
    // text, so a substring test on the command itself would be unreliable).
    let interesting = (line.contains("\"turn_context\"") && line.contains(turn_id))
        || (owner_command.is_some()
            && (line.contains("function_call") || line.contains("custom_tool_call")));
    if !interesting {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let item_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = value.get("payload").unwrap_or(&Value::Null);
    if item_type != "turn_context" && item_type != "response_item" {
        return;
    }
    if item_type == "turn_context" {
        if payload.get("turn_id").and_then(Value::as_str) == Some(turn_id) {
            // Keep the last matching turn context (mid-turn updates override).
            scan.turn_context = Some(payload.clone());
        }
        return;
    }
    let payload_type = payload.get("type").and_then(Value::as_str).unwrap_or("");
    let call_turn = payload
        .get("internal_chat_message_metadata_passthrough")
        .and_then(|metadata| metadata.get("turn_id"))
        .and_then(Value::as_str);
    // Missing turn metadata is treated conservatively as current-turn (D43/D44).
    let current_turn = call_turn.is_none() || call_turn == Some(turn_id);
    match payload_type {
        "function_call" => {
            let (Some(probe), true) = (owner_command, current_turn) else {
                return;
            };
            // FunctionCall arguments are a JSON string; shell runtimes carry the hook
            // command as `command` (shell) or `cmd` (unified exec).
            let Some(arguments) = payload
                .get("arguments")
                .and_then(Value::as_str)
                .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
            else {
                return;
            };
            if let Some(call) = owner_call_from_arguments(&arguments, probe) {
                scan.owner_calls.push(call);
            }
        }
        "custom_tool_call" => {
            // code_mode: the model drives tools from a JS cell; arguments live in the
            // source text and are extracted per exec_command call (D44).
            let input = payload.get("input").and_then(Value::as_str).unwrap_or("");
            let (Some(probe), true) = (owner_command, current_turn) else {
                return;
            };
            if payload.get("name").and_then(Value::as_str) != Some("exec") {
                return;
            }
            for arguments in exec_command_objects(input) {
                if let Some(call) = owner_call_from_arguments(&Value::Object(arguments), probe) {
                    scan.owner_calls.push(call);
                }
            }
        }
        _ => {}
    }
}

/// Owner-call fields when `arguments` carries the probed command as `command` (shell
/// runtimes) or `cmd` (unified exec); `None` when this call is not the owner.
fn owner_call_from_arguments(arguments: &Value, probe: &str) -> Option<OwnerCall> {
    let command = arguments
        .get("command")
        .and_then(Value::as_str)
        .or_else(|| arguments.get("cmd").and_then(Value::as_str));
    if command != Some(probe) {
        return None;
    }
    let prefix_rule = arguments.get("prefix_rule").and_then(|value| {
        let tokens = value.as_array()?;
        tokens
            .iter()
            .map(|token| token.as_str().map(str::to_string))
            .collect::<Option<Vec<String>>>()
    });
    Some(OwnerCall {
        justification: arguments
            .get("justification")
            .and_then(Value::as_str)
            .map(str::to_string),
        prefix_rule,
        // Present-but-non-string values map to a sentinel so they count as a
        // sandbox override (conservative) instead of the absent default.
        sandbox_permissions: arguments.get("sandbox_permissions").map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| "<non-string>".to_string())
        }),
    })
}

/// Strict extraction of every `tools.exec_command({...})` argument object from a
/// code_mode JS cell (one cell routinely runs several commands). Calls that do not
/// parse as double-quoted JSON-compatible literals are skipped: an unparseable call
/// can never *prove* ownership, so skipping only fails toward the basic popup.
fn exec_command_objects(source: &str) -> Vec<serde_json::Map<String, Value>> {
    const NEEDLE: &str = "tools.exec_command(";
    let mut objects = Vec::new();
    let mut cursor = 0;
    while let Some(found) = source[cursor..].find(NEEDLE) {
        cursor += found + NEEDLE.len();
        let mut parser = JsObjectParser {
            bytes: &source.as_bytes()[cursor..],
            pos: 0,
        };
        parser.skip_ws();
        let Some(object) = parser.parse_object(0) else {
            continue;
        };
        parser.skip_ws();
        if parser.eat(b',') {
            parser.skip_ws();
        }
        if !parser.eat(b')') {
            continue;
        }
        objects.push(object);
    }
    objects
}

/// Minimal recognizer for JS object literals restricted to JSON-expressible content:
/// identifier or double-quoted keys, double-quoted strings with JSON escapes, numbers,
/// booleans, null, arrays, and shallow nested objects. Single quotes, template strings,
/// expressions, and comments all fail the parse.
struct JsObjectParser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl JsObjectParser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn eat(&mut self, byte: u8) -> bool {
        if self.peek() == Some(byte) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.pos += 1;
        }
    }

    fn parse_object(&mut self, depth: usize) -> Option<serde_json::Map<String, Value>> {
        if depth > 4 || !self.eat(b'{') {
            return None;
        }
        let mut object = serde_json::Map::new();
        loop {
            self.skip_ws();
            if self.eat(b'}') {
                return Some(object);
            }
            let key = if self.peek() == Some(b'"') {
                self.parse_string()?
            } else {
                self.parse_ident()?
            };
            self.skip_ws();
            if !self.eat(b':') {
                return None;
            }
            self.skip_ws();
            let value = self.parse_value(depth)?;
            object.insert(key, value);
            self.skip_ws();
            if self.eat(b',') {
                continue;
            }
            if self.eat(b'}') {
                return Some(object);
            }
            return None;
        }
    }

    fn parse_value(&mut self, depth: usize) -> Option<Value> {
        match self.peek()? {
            b'"' => Some(Value::String(self.parse_string()?)),
            b'{' => Some(Value::Object(self.parse_object(depth + 1)?)),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.eat(b']') {
                        return Some(Value::Array(items));
                    }
                    items.push(self.parse_value(depth + 1)?);
                    self.skip_ws();
                    if self.eat(b',') {
                        continue;
                    }
                    if self.eat(b']') {
                        return Some(Value::Array(items));
                    }
                    return None;
                }
            }
            b'0'..=b'9' | b'-' => {
                let start = self.pos;
                while matches!(
                    self.peek(),
                    Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
                ) {
                    self.pos += 1;
                }
                let text = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
                serde_json::from_str::<Value>(text)
                    .ok()
                    .filter(Value::is_number)
            }
            _ => match self.parse_ident()?.as_str() {
                "true" => Some(Value::Bool(true)),
                "false" => Some(Value::Bool(false)),
                "null" => Some(Value::Null),
                _ => None,
            },
        }
    }

    /// JSON-compatible double-quoted string (JS escapes beyond JSON's set fail).
    fn parse_string(&mut self) -> Option<String> {
        let start = self.pos;
        if !self.eat(b'"') {
            return None;
        }
        loop {
            match self.peek()? {
                b'\\' => {
                    self.pos += 2;
                    if self.pos > self.bytes.len() {
                        return None;
                    }
                }
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\n' => return None,
                _ => self.pos += 1,
            }
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.pos]).ok()?;
        match serde_json::from_str::<Value>(raw) {
            Ok(Value::String(text)) => Some(text),
            _ => None,
        }
    }

    fn parse_ident(&mut self) -> Option<String> {
        let start = self.pos;
        while matches!(self.peek(), Some(byte) if byte.is_ascii_alphanumeric() || byte == b'_') {
            self.pos += 1;
        }
        if self.pos == start {
            return None;
        }
        std::str::from_utf8(&self.bytes[start..self.pos])
            .ok()
            .map(str::to_string)
    }
}

fn evaluate_gate(scan: &RolloutScan) -> RolloutGate {
    let Some(context) = scan.turn_context.as_ref() else {
        return RolloutGate::Unproven;
    };
    let reviewer = context
        .get("approvals_reviewer")
        .and_then(Value::as_str)
        .unwrap_or("user");
    let policy = context.get("approval_policy");
    let policy_guardian_eligible = match policy {
        Some(Value::String(value)) => value == "on-request" || value == "on-failure",
        Some(Value::Object(map)) => map.contains_key("granular"),
        _ => false,
    };
    match reviewer {
        "user" => RolloutGate::MemoryAllowed,
        "auto_review" | "guardian_subagent" if policy_guardian_eligible => {
            RolloutGate::GuardianProven
        }
        // Reviewer is not the user but routing is not provable: fail closed to basic.
        _ => RolloutGate::Unproven,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn write_rollout(lines: &[Value]) -> tempfile::NamedTempFile {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        for line in lines {
            writeln!(file, "{line}").unwrap();
        }
        file
    }

    fn turn_context(turn_id: &str, reviewer: Option<&str>, policy: Value) -> Value {
        let mut payload = json!({
            "turn_id": turn_id,
            "approval_policy": policy,
        });
        if let Some(reviewer) = reviewer {
            payload["approvals_reviewer"] = json!(reviewer);
        }
        json!({ "timestamp": "t", "type": "turn_context", "payload": payload })
    }

    fn hook_input(transcript: &std::path::Path, patch: &str) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s1",
            "turn_id": "turn-1",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": "/work/proj",
            "permission_mode": "default",
            "tool_name": "apply_patch",
            "tool_input": { "command": patch },
        })
    }

    const PATCH: &str = "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** End Patch\n";

    #[test]
    fn user_reviewer_enables_memory_options() {
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let analysis = analyze_codex(&hook_input(rollout.path(), PATCH), false);
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analysis
        else {
            panic!("expected enhancement");
        };
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::FileEdit { ref paths }) if paths == &["/work/proj/src/a.rs"]
        ));
        // No git root exists above the fake cwd, so D12 falls back to cwd as the project
        // root and every path is inside it -> project option.
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, [ACTION_REMEMBER_FILES, ACTION_REMEMBER_PROJECT]);
        assert_eq!(memory.saves.len(), 2);
    }

    #[test]
    fn paths_outside_project_root_offer_full_disk() {
        let patch = "*** Begin Patch\n*** Update File: /etc/hosts\n@@\n-x\n+y\n*** End Patch\n";
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex(&hook_input(rollout.path(), patch), false)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, [ACTION_REMEMBER_FILES, ACTION_REMEMBER_DISK]);
        let disk_save = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_REMEMBER_DISK)
            .unwrap();
        assert_eq!(disk_save.rules, vec![RuleKey::FileDisk]);
        // Danger styling for the full-disk option (§4.3).
        assert_eq!(extra_choices[1].role, ActionRole::Destructive);
    }

    #[test]
    fn absent_reviewer_defaults_to_user() {
        let rollout = write_rollout(&[turn_context("turn-1", None, json!("untrusted"))]);
        assert!(matches!(
            analyze_codex(&hook_input(rollout.path(), PATCH), true),
            Analysis::Enhanced { .. }
        ));
    }

    #[test]
    fn proven_guardian_suppresses_popup() {
        for policy in [json!("on-request"), json!({"granular": {}})] {
            let rollout = write_rollout(&[turn_context("turn-1", Some("auto_review"), policy)]);
            assert!(matches!(
                analyze_codex(&hook_input(rollout.path(), PATCH), false),
                Analysis::Suppress
            ));
        }
    }

    #[test]
    fn guardian_with_other_policy_is_unproven_basic() {
        let rollout = write_rollout(&[turn_context("turn-1", Some("auto_review"), json!("never"))]);
        assert!(matches!(
            analyze_codex(&hook_input(rollout.path(), PATCH), false),
            Analysis::Basic
        ));
    }

    #[test]
    fn request_permissions_in_turn_no_longer_disables_memory() {
        // D43 relaxed (D50): upstream #32232 makes hook decisions authoritative before
        // the automated reviewer, so request_permissions turns behave like normal turns.
        let call = json!({
            "timestamp": "t",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "request_permissions",
                "arguments": "{}",
                "call_id": "c1",
                "internal_chat_message_metadata_passthrough": { "turn_id": "turn-1" }
            }
        });
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            call,
        ]);
        assert!(matches!(
            analyze_codex(&hook_input(rollout.path(), PATCH), false),
            Analysis::Enhanced { .. }
        ));
    }

    #[test]
    fn missing_rollout_or_turn_context_keeps_basic() {
        let input = json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s1",
            "turn_id": "turn-1",
            "transcript_path": "/no/such/rollout.jsonl",
            "cwd": "/work/proj",
            "permission_mode": "default",
            "tool_name": "apply_patch",
            "tool_input": { "command": PATCH },
        });
        assert!(matches!(analyze_codex(&input, false), Analysis::Basic));

        // Rollout exists but has a different turn's context only.
        let rollout = write_rollout(&[turn_context("turn-0", Some("user"), json!("on-request"))]);
        assert!(matches!(
            analyze_codex(&hook_input(rollout.path(), PATCH), false),
            Analysis::Basic
        ));

        // No transcript_path at all.
        let mut no_path = hook_input(std::path::Path::new("/tmp/x"), PATCH);
        no_path.as_object_mut().unwrap().remove("transcript_path");
        assert!(matches!(analyze_codex(&no_path, false), Analysis::Basic));
    }

    #[test]
    fn malformed_patch_keeps_basic() {
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let analysis = analyze_codex(
            &hook_input(rollout.path(), "*** Begin Patch\n*** Add File: x\n+x\n"),
            false,
        );
        assert!(matches!(analysis, Analysis::Basic));
    }

    #[test]
    fn unsupported_tools_stay_basic() {
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let mut input = hook_input(rollout.path(), PATCH);
        input["tool_name"] = json!("web_search");
        input["tool_input"] = json!({"query": "weather"});
        assert!(matches!(analyze_codex(&input, false), Analysis::Basic));
    }

    // ===== built-in AskHuman self-call whitelist (D49) =====

    /// A fake executable standing in for the installed AskHuman binary.
    fn fake_exe(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn self_call_usage_covers_ask_shapes_only() {
        let ok = |args: &[&str]| {
            let args: Vec<String> = args.iter().map(ToString::to_string).collect();
            self_call_usage_allowed(&args)
        };
        assert!(ok(&["--agent-help"]));
        assert!(ok(&["--show-last"]));
        assert!(ok(&["--show-last", "5"]));
        assert!(ok(&["todo", "add", "review the deploy plan"]));
        assert!(ok(&[
            "--whats-next",
            "done",
            "-o!",
            "next task",
            "-f",
            "report.md"
        ]));
        assert!(ok(&["--whats-next", "--stdin"]));
        assert!(ok(&[
            "看看这个改动？",
            "-f",
            "./diff.patch",
            "-q",
            "继续吗？",
            "-o",
            "继续"
        ]));
        assert!(ok(&["-q", "要继续部署吗？", "-o!", "继续", "-o", "停止"]));

        assert!(!ok(&[]));
        assert!(!ok(&["--agent-help", "extra"]));
        assert!(!ok(&["--show-last", "extra"]));
        assert!(!ok(&["--show-last", "0"]));
        assert!(!ok(&["--show-last", "11"]));
        assert!(!ok(&["todo", "list"]));
        assert!(!ok(&["todo", "add"]));
        assert!(!ok(&["--whats-next", "-q", "smuggled question"]));
        assert!(!ok(&["--settings"]));
        assert!(!ok(&["daemon", "stop"]));
        assert!(!ok(&["config", "set", "x"]));
        assert!(!ok(&["-q", "x", "--unknown-flag"]));
    }

    #[test]
    fn shell_self_call_requires_real_binary_identity() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = fake_exe(&root, "AskHuman");
        let impostor_dir = root.join("workspace");
        std::fs::create_dir_all(&impostor_dir).unwrap();
        let impostor = fake_exe(&impostor_dir, "AskHuman");
        let cwd = impostor_dir.to_string_lossy().to_string();
        let path_var = std::ffi::OsString::from(root.as_os_str());

        // Absolute path to the installed binary.
        let script = format!("{} -q \"continue?\" -o yes", exe.display());
        assert!(shell_self_call_at(&script, &cwd, &exe, Some(&path_var)));
        // Bare name resolving through an absolute PATH entry.
        assert!(shell_self_call_at(
            "AskHuman --agent-help",
            &cwd,
            &exe,
            Some(&path_var)
        ));
        // Symlink to the installed binary canonicalizes to the same identity.
        #[cfg(unix)]
        {
            let link = root.join("ah-link");
            std::os::unix::fs::symlink(&exe, &link).unwrap();
            let script = format!("{} --agent-help", link.display());
            assert!(shell_self_call_at(&script, &cwd, &exe, Some(&path_var)));
        }

        // A workspace-planted binary (relative or absolute) is not us.
        assert!(!shell_self_call_at(
            "./AskHuman --agent-help",
            &cwd,
            &exe,
            Some(&path_var)
        ));
        let script = format!("{} --agent-help", impostor.display());
        assert!(!shell_self_call_at(&script, &cwd, &exe, Some(&path_var)));
        // Relative PATH entries never resolve.
        let loose_path = std::ffi::OsString::from(".");
        assert!(!shell_self_call_at(
            "AskHuman --agent-help",
            &cwd,
            &exe,
            Some(&loose_path)
        ));
        // Non-ask usage of the genuine binary still pops up.
        let script = format!("{} daemon stop", exe.display());
        assert!(!shell_self_call_at(&script, &cwd, &exe, Some(&path_var)));
        // Compound scripts never ride the whitelist.
        let script = format!("{} --agent-help && rm -rf /", exe.display());
        assert!(!shell_self_call_at(&script, &cwd, &exe, Some(&path_var)));
    }

    #[test]
    fn shell_self_call_auto_allows_through_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = fake_exe(&root, "AskHuman");
        // Guardian-suppressed turns stay suppressed even for self-calls.
        let rollout = write_rollout(&[turn_context(
            "turn-1",
            Some("auto_review"),
            json!("on-request"),
        )]);
        let mut input = hook_input(rollout.path(), PATCH);
        input["tool_name"] = json!("Bash");
        input["tool_input"] = json!({ "command": format!("{} --agent-help", exe.display()) });
        assert!(matches!(analyze_codex(&input, false), Analysis::Suppress));
        // Note: the AutoAllow half of the flow needs current_exe to be the hook binary,
        // which unit tests cannot fake; identity coverage lives in
        // `shell_self_call_requires_real_binary_identity`.
    }

    #[test]
    fn mcp_self_call_requires_matching_server_command() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = fake_exe(&root, "AskHuman");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        let cwd = root.to_string_lossy().to_string();
        let write_config = |command: &str, extra: &str| {
            let command = toml_edit::Value::from(command).to_string();
            std::fs::write(
                codex_home.join("config.toml"),
                format!("[mcp_servers.askhuman]\ncommand = {command}\nargs = [\"mcp\"]\n{extra}"),
            )
            .unwrap();
        };

        write_config(&exe.to_string_lossy(), "");
        assert!(mcp_self_call_at(
            "mcp__askhuman__ask",
            &cwd,
            &codex_home,
            &exe
        ));
        assert!(mcp_self_call_at(
            "mcp__askhuman__whats_next",
            &cwd,
            &codex_home,
            &exe
        ));
        assert!(mcp_self_call_at(
            "mcp__askhuman__todo_add",
            &cwd,
            &codex_home,
            &exe
        ));
        // Other tools of the server (none exist today) and other servers never match.
        assert!(!mcp_self_call_at(
            "mcp__askhuman__evil",
            &cwd,
            &codex_home,
            &exe
        ));
        assert!(!mcp_self_call_at(
            "mcp__other__ask",
            &cwd,
            &codex_home,
            &exe
        ));

        // Explicit prompt mode configured by the user wins.
        write_config(
            &exe.to_string_lossy(),
            "default_tools_approval_mode = \"prompt\"\n",
        );
        assert!(!mcp_self_call_at(
            "mcp__askhuman__ask",
            &cwd,
            &codex_home,
            &exe
        ));

        // A server pointing at a different binary is not us.
        let impostor = fake_exe(&root, "NotAskHuman");
        write_config(&impostor.to_string_lossy(), "");
        assert!(!mcp_self_call_at(
            "mcp__askhuman__ask",
            &cwd,
            &codex_home,
            &exe
        ));

        // Relative command strings are unresolvable -> fail closed.
        write_config("AskHuman", "");
        assert!(!mcp_self_call_at(
            "mcp__askhuman__ask",
            &cwd,
            &codex_home,
            &exe
        ));

        // No layer defines the server at all -> no whitelist.
        std::fs::remove_file(codex_home.join("config.toml")).unwrap();
        assert!(!mcp_self_call_at(
            "mcp__askhuman__ask",
            &cwd,
            &codex_home,
            &exe
        ));
    }

    fn claude_input(tool_name: &str, tool_input: Value, cwd: &str) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s1",
            "cwd": cwd,
            "permission_mode": "default",
            "tool_name": tool_name,
            "tool_input": tool_input,
        })
    }

    #[test]
    fn claude_shell_self_call_honors_identity_and_explicit_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = fake_exe(&root, "AskHuman");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let cwd = project.to_string_lossy().to_string();
        let path_var = std::ffi::OsString::from(root.as_os_str());

        let script = format!("{} --whats-next \"done, next?\"", exe.display());
        let input = claude_input("Bash", json!({ "command": script }), &cwd);
        assert!(claude_self_call_at(&input, &home, &exe, Some(&path_var)));

        // Non-ask usage / impostor binary stay on the popup.
        let impostor = fake_exe(&project, "AskHuman");
        let bad = claude_input(
            "Bash",
            json!({ "command": format!("{} daemon stop", exe.display()) }),
            &cwd,
        );
        assert!(!claude_self_call_at(&bad, &home, &exe, Some(&path_var)));
        let bad = claude_input(
            "Bash",
            json!({ "command": format!("{} --agent-help", impostor.display()) }),
            &cwd,
        );
        assert!(!claude_self_call_at(&bad, &home, &exe, Some(&path_var)));

        // An explicit user ask rule mentioning AskHuman wins over the whitelist.
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{ "permissions": { "ask": ["Bash(AskHuman:*)"] } }"#,
        )
        .unwrap();
        let input = claude_input(
            "Bash",
            json!({ "command": format!("{} --agent-help", exe.display()) }),
            &cwd,
        );
        assert!(!claude_self_call_at(&input, &home, &exe, Some(&path_var)));
        // Rules about other tools do not interfere.
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{ "permissions": { "ask": ["Bash(rm:*)"], "deny": ["mcp__other"] } }"#,
        )
        .unwrap();
        assert!(claude_self_call_at(&input, &home, &exe, Some(&path_var)));
        // Project-level explicit rules count too.
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(
            project.join(".claude/settings.local.json"),
            r#"{ "permissions": { "deny": ["Bash(AskHuman daemon:*)"] } }"#,
        )
        .unwrap();
        assert!(!claude_self_call_at(&input, &home, &exe, Some(&path_var)));
    }

    #[test]
    fn claude_mcp_self_call_verifies_every_defining_layer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = fake_exe(&root, "AskHuman");
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(".git")).unwrap();
        let cwd = project.to_string_lossy().to_string();
        let input = claude_input("mcp__askhuman__ask", json!({ "questions": [] }), &cwd);

        // No layer defines the server -> no whitelist.
        assert!(!claude_self_call_at(&input, &home, &exe, None));

        // User-scope definition pointing at us.
        let user_config = |command: &str| {
            std::fs::write(
                home.join(".claude.json"),
                json!({ "mcpServers": { "askhuman": { "command": command, "args": ["mcp"] } } })
                    .to_string(),
            )
            .unwrap();
        };
        user_config(&exe.to_string_lossy());
        assert!(claude_self_call_at(&input, &home, &exe, None));
        let whats_next = claude_input("mcp__askhuman__whats_next", json!({}), &cwd);
        assert!(claude_self_call_at(&whats_next, &home, &exe, None));
        let todo_add = claude_input("mcp__askhuman__todo_add", json!({ "text": "x" }), &cwd);
        assert!(claude_self_call_at(&todo_add, &home, &exe, None));
        // Other tools / servers never match.
        let other = claude_input("mcp__askhuman__evil", json!({}), &cwd);
        assert!(!claude_self_call_at(&other, &home, &exe, None));
        let other = claude_input("mcp__other__ask", json!({}), &cwd);
        assert!(!claude_self_call_at(&other, &home, &exe, None));

        // A workspace `.mcp.json` impostor kills the whitelist even with a good user layer.
        let impostor = fake_exe(&root, "Impostor");
        std::fs::write(
            project.join(".mcp.json"),
            json!({ "mcpServers": { "askhuman": { "command": impostor.to_string_lossy() } } })
                .to_string(),
        )
        .unwrap();
        assert!(!claude_self_call_at(&input, &home, &exe, None));
        std::fs::remove_file(project.join(".mcp.json")).unwrap();

        // Local scope (projects entry covering the cwd) is also verified.
        std::fs::write(
            home.join(".claude.json"),
            json!({
                "mcpServers": { "askhuman": { "command": exe.to_string_lossy() } },
                "projects": {
                    project.to_string_lossy().as_ref(): {
                        "mcpServers": { "askhuman": { "command": impostor.to_string_lossy() } }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(!claude_self_call_at(&input, &home, &exe, None));
        // A projects entry for an unrelated directory is ignored.
        std::fs::write(
            home.join(".claude.json"),
            json!({
                "mcpServers": { "askhuman": { "command": exe.to_string_lossy() } },
                "projects": {
                    "/elsewhere": {
                        "mcpServers": { "askhuman": { "command": impostor.to_string_lossy() } }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        assert!(claude_self_call_at(&input, &home, &exe, None));

        // Relative command strings are unresolvable -> fail closed.
        user_config("AskHuman");
        assert!(!claude_self_call_at(&input, &home, &exe, None));

        // An explicit ask rule for the MCP tool wins.
        user_config(&exe.to_string_lossy());
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(
            home.join(".claude/settings.json"),
            r#"{ "permissions": { "ask": ["mcp__askhuman__ask"] } }"#,
        )
        .unwrap();
        assert!(!claude_self_call_at(&input, &home, &exe, None));
    }

    #[test]
    fn mcp_self_call_rejects_workspace_defined_impostor_layer() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let exe = fake_exe(&root, "AskHuman");
        let codex_home = root.join("codex-home");
        std::fs::create_dir_all(&codex_home).unwrap();
        std::fs::write(
            codex_home.join("config.toml"),
            format!(
                "[mcp_servers.askhuman]\ncommand = {}\n",
                toml_edit::Value::from(exe.to_string_lossy().as_ref())
            ),
        )
        .unwrap();
        // A project layer redefines `askhuman` with another command: every defining
        // layer must point at us, so the whitelist stays off.
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(".codex")).unwrap();
        let impostor = fake_exe(&root, "Impostor");
        std::fs::write(
            project.join(".codex/config.toml"),
            format!(
                "[mcp_servers.askhuman]\ncommand = {}\n",
                toml_edit::Value::from(impostor.to_string_lossy().as_ref())
            ),
        )
        .unwrap();
        let cwd = project.to_string_lossy().to_string();
        assert!(!mcp_self_call_at(
            "mcp__askhuman__ask",
            &cwd,
            &codex_home,
            &exe
        ));
    }

    #[test]
    fn project_option_appears_when_all_paths_inside_git_root() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let mut input = hook_input(rollout.path(), PATCH);
        input["cwd"] = json!(root.join("sub").to_string_lossy());
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex(&input, true)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, [ACTION_REMEMBER_FILES, ACTION_REMEMBER_PROJECT]);
        let project_save = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_REMEMBER_PROJECT)
            .unwrap();
        assert!(matches!(
            project_save.rules.as_slice(),
            [RuleKey::FileProject { .. }]
        ));
    }

    // ===== MCP (D40/D41) =====

    fn assert_shadow_permanent(saves: &[MemorySave]) {
        let always = saves
            .iter()
            .find(|save| save.action_id == ACTION_MCP_ALWAYS)
            .unwrap();
        assert_eq!(
            always.namespace,
            crate::permission_rules::RuleNamespace::Global
        );
        assert!(always.native.is_none());
    }

    #[test]
    fn mcp_without_native_channel_offers_session_plus_shadow() {
        let home = tempfile::tempdir().unwrap();
        let (memory, choices) = mcp_enhancement_at(
            "mcp__github__create_issue",
            "/work/proj",
            false,
            home.path(),
        )
        .unwrap();
        let ids: Vec<&str> = choices.iter().map(|choice| choice.id.as_str()).collect();
        assert_eq!(ids, [ACTION_MCP_SESSION, ACTION_MCP_ALWAYS]);
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::McpTool { ref tool }) if tool == "mcp__github__create_issue"
        ));
        let session = &memory.saves[0];
        assert_eq!(
            session.namespace,
            crate::permission_rules::RuleNamespace::Session
        );
        assert_eq!(
            session.rules,
            vec![RuleKey::McpTool {
                tool: "mcp__github__create_issue".into()
            }]
        );
        assert_shadow_permanent(&memory.saves);
    }

    #[test]
    fn mcp_user_config_server_gets_native_write() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.github]\ncommand = \"gh-mcp\"\n",
        )
        .unwrap();
        let (memory, _) =
            mcp_enhancement_at("mcp__github__create_issue", "/work/proj", true, home.path())
                .unwrap();
        let always = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_MCP_ALWAYS)
            .unwrap();
        assert_eq!(
            always.native,
            Some(NativeWrite::McpApprovalMode {
                config_path: home
                    .path()
                    .join("config.toml")
                    .to_string_lossy()
                    .to_string(),
                server: "github".into(),
                tool: "create_issue".into(),
            })
        );
        // Session bridge rule rides along with the native write (D40).
        assert_eq!(
            always.namespace,
            crate::permission_rules::RuleNamespace::Session
        );
        assert_eq!(
            always.rules,
            vec![RuleKey::McpTool {
                tool: "mcp__github__create_issue".into()
            }]
        );
    }

    #[test]
    fn mcp_explicit_prompt_or_writes_hides_all_options() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.github]\ncommand = \"gh-mcp\"\n[mcp_servers.github.tools.create_issue]\napproval_mode = \"prompt\"\n",
        )
        .unwrap();
        assert!(mcp_enhancement_at(
            "mcp__github__create_issue",
            "/work/proj",
            false,
            home.path()
        )
        .is_none());

        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.github]\ncommand = \"gh-mcp\"\ndefault_tools_approval_mode = \"writes\"\n",
        )
        .unwrap();
        assert!(mcp_enhancement_at(
            "mcp__github__create_issue",
            "/work/proj",
            false,
            home.path()
        )
        .is_none());

        // Tool-level explicit auto overrides the server default: options stay.
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.github]\ncommand = \"gh-mcp\"\ndefault_tools_approval_mode = \"writes\"\n[mcp_servers.github.tools.create_issue]\napproval_mode = \"auto\"\n",
        )
        .unwrap();
        assert!(mcp_enhancement_at(
            "mcp__github__create_issue",
            "/work/proj",
            false,
            home.path()
        )
        .is_some());
    }

    #[test]
    fn mcp_ambiguous_server_split_falls_back_to_shadow() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.a]\ncommand = \"a\"\n[mcp_servers.a__b]\ncommand = \"ab\"\n",
        )
        .unwrap();
        let (memory, _) =
            mcp_enhancement_at("mcp__a__b__c", "/work/proj", false, home.path()).unwrap();
        assert_shadow_permanent(&memory.saves);
    }

    #[test]
    fn mcp_codex_apps_always_uses_shadow() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.codex_apps]\ncommand = \"x\"\n",
        )
        .unwrap();
        let (memory, _) = mcp_enhancement_at(
            "mcp__codex_apps__calendar__create_event",
            "/work/proj",
            false,
            home.path(),
        )
        .unwrap();
        assert_shadow_permanent(&memory.saves);
    }

    #[test]
    fn mcp_trusted_project_layer_wins_untrusted_falls_through() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::write(
            root.join(".codex/config.toml"),
            "[mcp_servers.github]\ncommand = \"proj\"\n",
        )
        .unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.github]\ncommand = \"user\"\n",
        )
        .unwrap();
        let cwd = root.to_string_lossy().to_string();

        // Untrusted project: the project layer is never a write target; user layer wins.
        let (memory, _) =
            mcp_enhancement_at("mcp__github__create_issue", &cwd, false, home.path()).unwrap();
        let always = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_MCP_ALWAYS)
            .unwrap();
        let Some(NativeWrite::McpApprovalMode { config_path, .. }) = &always.native else {
            panic!("expected native write");
        };
        assert!(config_path.ends_with("config.toml"));
        assert!(!config_path.contains(".codex/config.toml"));

        // Mark the project trusted (raw key): project config becomes the target.
        let trusted = format!(
            "[mcp_servers.github]\ncommand = \"user\"\n[projects.{}]\ntrust_level = \"trusted\"\n",
            toml_edit::Value::from(root.to_string_lossy().as_ref())
        );
        std::fs::write(home.path().join("config.toml"), trusted).unwrap();
        let (memory, _) =
            mcp_enhancement_at("mcp__github__create_issue", &cwd, false, home.path()).unwrap();
        let always = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_MCP_ALWAYS)
            .unwrap();
        let Some(NativeWrite::McpApprovalMode { config_path, .. }) = &always.native else {
            panic!("expected native write");
        };
        assert!(config_path.ends_with(".codex/config.toml"));
    }

    #[test]
    fn mcp_requests_go_through_rollout_gate() {
        // Guardian-proven rollouts suppress MCP popups too. The improbable server name keeps
        // the test independent from the developer's real ~/.codex/config.toml.
        let rollout = write_rollout(&[turn_context(
            "turn-1",
            Some("auto_review"),
            json!("on-request"),
        )]);
        let mut input = hook_input(rollout.path(), PATCH);
        input["tool_name"] = json!("mcp__askhuman_test_no_such_server__tool");
        input["tool_input"] = json!({});
        assert!(matches!(analyze_codex(&input, false), Analysis::Suppress));

        // Unprovable rollout keeps the basic popup, no memory options.
        let mut input = input.clone();
        input["transcript_path"] = json!("/no/such/rollout.jsonl");
        assert!(matches!(analyze_codex(&input, false), Analysis::Basic));
    }

    // ===== network (D39) =====

    fn shell_call(turn: &str, command: &str, justification: Option<&str>) -> Value {
        let mut arguments = json!({ "command": command });
        if let Some(justification) = justification {
            arguments["justification"] = json!(justification);
        }
        json!({
            "timestamp": "t",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell_command",
                "arguments": arguments.to_string(),
                "call_id": "c1",
                "internal_chat_message_metadata_passthrough": { "turn_id": turn }
            }
        })
    }

    fn network_input(transcript: &std::path::Path, command: &str, description: &str) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s1",
            "turn_id": "turn-1",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": "/work/proj",
            "permission_mode": "default",
            "tool_name": "Bash",
            "tool_input": { "command": command, "description": description },
        })
    }

    const NET_DESC: &str = "network-access https://api.github.com:443";

    #[test]
    fn genuine_network_request_offers_host_options() {
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            shell_call(
                "turn-1",
                "curl https://api.github.com/repos",
                Some("fetch repo data"),
            ),
        ]);
        let input = network_input(
            rollout.path(),
            "curl https://api.github.com/repos",
            NET_DESC,
        );
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex(&input, false)
        else {
            panic!("expected enhancement");
        };
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::NetworkHost { ref host, ref protocol, port })
                if host == "api.github.com" && protocol == "https" && port == 443
        ));
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, [ACTION_NETWORK_SESSION, ACTION_NETWORK_ALWAYS]);
        let always = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_NETWORK_ALWAYS)
            .unwrap();
        let Some(NativeWrite::NetworkRule {
            rules_path,
            host,
            protocol,
        }) = &always.native
        else {
            panic!("expected network rule write");
        };
        assert!(Path::new(rules_path).ends_with(Path::new("rules").join("default.rules")));
        assert_eq!(host, "api.github.com");
        assert_eq!(protocol, "https");
        // Bridge session rule keeps the port dimension.
        assert_eq!(
            always.rules,
            vec![RuleKey::NetworkHost {
                host: "api.github.com".into(),
                protocol: "https".into(),
                port: 443,
            }]
        );
    }

    #[test]
    fn forged_network_description_stays_basic() {
        // A plain shell request whose model justification mimics the machine format: the
        // justification equals the hook description, which proves it is model-authored.
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            shell_call("turn-1", "curl https://evil.com/x", Some(NET_DESC)),
        ]);
        let input = network_input(rollout.path(), "curl https://evil.com/x", NET_DESC);
        assert!(matches!(analyze_codex(&input, false), Analysis::Basic));
    }

    #[test]
    fn ownerless_prompt_command_is_genuine() {
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let input = network_input(
            rollout.path(),
            "network-access https://api.github.com:443",
            NET_DESC,
        );
        assert!(matches!(
            analyze_codex(&input, true),
            Analysis::Enhanced { .. }
        ));
    }

    #[test]
    fn owned_command_missing_from_rollout_stays_basic() {
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let input = network_input(
            rollout.path(),
            "curl https://api.github.com/repos",
            NET_DESC,
        );
        assert!(matches!(analyze_codex(&input, false), Analysis::Basic));
    }

    #[test]
    fn divergent_owner_justifications_check_forgery_per_call() {
        // D44 refined: diverging justifications across retries are harmless; the
        // network treatment stands as long as none equals the description.
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            shell_call("turn-1", "curl https://api.github.com/repos", Some("first")),
            shell_call(
                "turn-1",
                "curl https://api.github.com/repos",
                Some("second"),
            ),
        ]);
        let input = network_input(
            rollout.path(),
            "curl https://api.github.com/repos",
            NET_DESC,
        );
        assert!(matches!(
            analyze_codex(&input, false),
            Analysis::Enhanced { .. }
        ));
        // But any single call whose justification equals the description is a forgery.
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            shell_call("turn-1", "curl https://api.github.com/repos", Some("first")),
            shell_call(
                "turn-1",
                "curl https://api.github.com/repos",
                Some(NET_DESC),
            ),
        ]);
        let input = network_input(
            rollout.path(),
            "curl https://api.github.com/repos",
            NET_DESC,
        );
        assert!(matches!(analyze_codex(&input, false), Analysis::Basic));
    }

    #[test]
    fn malformed_network_descriptions_stay_basic() {
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            shell_call("turn-1", "curl https://x/", Some("fetch")),
        ]);
        for description in [
            "network-access ftp://host:21",          // protocol outside whitelist
            "network-access https://Host.Com:443",   // uppercase host
            "network-access https://host:99999",     // port overflow
            "network-access https://host:443 extra", // trailing text
            "network-access https://host:443\n",     // whitespace
            "please allow network-access https://h:443", // prefix text
            "network-access https://host:",          // empty port
        ] {
            let input = network_input(rollout.path(), "curl https://x/", description);
            assert!(
                matches!(analyze_codex(&input, false), Analysis::Basic),
                "should stay basic: {description:?}"
            );
        }
    }

    #[test]
    fn network_request_in_guardian_turn_is_suppressed() {
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("auto_review"), json!("on-request")),
            shell_call("turn-1", "curl https://api.github.com/repos", Some("fetch")),
        ]);
        let input = network_input(
            rollout.path(),
            "curl https://api.github.com/repos",
            NET_DESC,
        );
        assert!(matches!(analyze_codex(&input, false), Analysis::Suppress));
    }

    // ===== Plain shell (D38/D42/D44) =====

    use crate::permission_shell::{ShellProbe, ShellWorkerOutput};

    fn shell_turn_context(turn_id: &str, policy: Value, sandbox_kind: &str) -> Value {
        json!({
            "timestamp": "t",
            "type": "turn_context",
            "payload": {
                "turn_id": turn_id,
                "approval_policy": policy,
                "approvals_reviewer": "user",
                "file_system_sandbox_policy": { "kind": sandbox_kind },
            }
        })
    }

    fn shell_input(transcript: &std::path::Path, command: &str) -> Value {
        json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "s1",
            "turn_id": "turn-1",
            "transcript_path": transcript.to_string_lossy(),
            "cwd": "/work/proj",
            "permission_mode": "default",
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
    }

    fn shell_call_full(
        turn: &str,
        command: &str,
        prefix_rule: Option<Value>,
        sandbox_permissions: Option<&str>,
    ) -> Value {
        let mut arguments = json!({ "command": command });
        if let Some(prefix_rule) = prefix_rule {
            arguments["prefix_rule"] = prefix_rule;
        }
        if let Some(sandbox_permissions) = sandbox_permissions {
            arguments["sandbox_permissions"] = json!(sandbox_permissions);
        }
        json!({
            "timestamp": "t",
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "name": "shell_command",
                "arguments": arguments.to_string(),
                "call_id": "c1",
                "internal_chat_message_metadata_passthrough": { "turn_id": turn }
            }
        })
    }

    fn worker_output(segments: &[&[&str]]) -> ShellWorkerOutput {
        let segments: Vec<Vec<String>> = segments
            .iter()
            .map(|argv| argv.iter().map(|token| token.to_string()).collect())
            .collect();
        ShellWorkerOutput {
            disabled_reason: None,
            segment_allows: vec![false; segments.len()],
            segments,
            decision: "prompt".into(),
            explicit_allow_all: false,
            dangerous_any: false,
            amendment: None,
            amendment_from_prefix_rule: false,
            amendment_candidates: Vec::new(),
            policy_prompt_any: false,
        }
    }

    /// Sets the amendment plus the worker invariant: candidates always contain at least
    /// the base amendment (`analyze` fills them together).
    fn with_amendment(mut output: ShellWorkerOutput, prefix: &[&str]) -> ShellWorkerOutput {
        let prefix: Vec<String> = prefix.iter().map(|token| token.to_string()).collect();
        output.amendment = Some(prefix.clone());
        output.amendment_candidates = vec![prefix];
        output
    }

    #[test]
    fn plain_shell_offers_prefix_first_session_tier_and_permanent_amendment() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "git status && cargo build", None, None),
        ]);
        let input = shell_input(rollout.path(), "git status && cargo build");
        let runner = |_probe: &ShellProbe| {
            Some(with_amendment(
                worker_output(&[&["git", "status"], &["cargo", "build"]]),
                &["cargo", "build"],
            ))
        };
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex_with(&input, false, &runner)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        // The fallback-derived amendment feeds the session prefix tier and the permanent
        // tier; the exact tier is hidden when a prefix exists (D38 refined). Eligible
        // requests additionally offer the relaxed-mode opt-in (D52).
        assert_eq!(
            ids,
            [
                ACTION_SHELL_PREFIX,
                ACTION_SHELL_ALWAYS,
                ACTION_SHELL_RELAXED
            ]
        );
        // Single-candidate ladders stay plain choices (no selector metadata).
        assert!(extra_choices.iter().all(|choice| choice.variant.is_none()));
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::ShellCommands { ref commands, .. })
                if commands == &[vec!["git".to_string(), "status".into()],
                                 vec!["cargo".to_string(), "build".into()]]
        ));
        let session = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_SHELL_PREFIX)
            .unwrap();
        assert_eq!(
            session.rules,
            vec![RuleKey::ShellPrefix {
                prefix: vec!["cargo".into(), "build".into()]
            }]
        );
        assert!(session.native.is_none());
        let always = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_SHELL_ALWAYS)
            .unwrap();
        // No session bridge for shell permanents: default.rules is re-read next time (D28).
        assert!(always.rules.is_empty());
        assert!(matches!(
            always.native,
            Some(crate::permission_rules::NativeWrite::PrefixRule { ref prefix, ref rules_path })
                if prefix == &["cargo".to_string(), "build".into()]
                    && Path::new(rules_path)
                        .ends_with(Path::new("rules").join("default.rules"))
        ));
        // The plain label carries no implementation detail.
        let always_choice = extra_choices
            .iter()
            .find(|choice| choice.id == ACTION_SHELL_ALWAYS)
            .unwrap();
        assert_eq!(
            always_choice.label,
            "Always allow commands starting with cargo build"
        );
    }

    #[test]
    fn exact_session_tier_appears_only_without_a_prefix() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "git status", None, None),
        ]);
        let input = shell_input(rollout.path(), "git status");
        let runner = |_probe: &ShellProbe| Some(worker_output(&[&["git", "status"]]));
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex_with(&input, false, &runner)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        assert_eq!(ids, [ACTION_SHELL_SESSION, ACTION_SHELL_RELAXED]);
        let session = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_SHELL_SESSION)
            .unwrap();
        assert_eq!(
            session.rules,
            vec![RuleKey::ShellExact {
                argv: vec!["git".into(), "status".into()]
            }]
        );
    }

    #[test]
    fn exec_command_objects_extract_strictly() {
        // Real code_mode cell shape: newlines, bare keys, arrays, trailing statements.
        let source = "const r = await tools.exec_command({\n  cmd: \"curl -sS https://example.com\",\n  workdir: \"/w\",\n  yield_time_ms: 10000,\n  sandbox_permissions: \"require_escalated\",\n  justification: \"why not\",\n  prefix_rule: [\"curl\", \"-sS\"],\n});\ntext(JSON.stringify(r));\n";
        let objects = exec_command_objects(source);
        assert_eq!(objects.len(), 1);
        let object = &objects[0];
        assert_eq!(
            object.get("cmd").and_then(Value::as_str),
            Some("curl -sS https://example.com")
        );
        assert_eq!(object.get("prefix_rule"), Some(&json!(["curl", "-sS"])),);
        assert_eq!(
            object.get("justification").and_then(Value::as_str),
            Some("why not")
        );
        // Quoted keys, escapes, and nested objects parse; JSON semantics preserved.
        let objects = exec_command_objects(
            "tools.exec_command({\"cmd\": \"echo \\\"hi\\\"\", env: { A: \"1\" }, ok: true })",
        );
        assert_eq!(
            objects[0].get("cmd").and_then(Value::as_str),
            Some("echo \"hi\"")
        );
        // A cell driving several commands yields every parseable call.
        let objects = exec_command_objects(
            "await tools.exec_command({cmd: \"a\"});\nawait tools.exec_command({cmd: \"b\"});",
        );
        assert_eq!(
            objects
                .iter()
                .filter_map(|object| object.get("cmd").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            ["a", "b"]
        );

        // Non-JSON syntax is skipped per call (never a false owner).
        for bad in [
            // Single-quoted / template strings.
            "tools.exec_command({cmd: 'a'})",
            "tools.exec_command({cmd: `a`})",
            // Expressions as values.
            "tools.exec_command({cmd: someVar})",
            "tools.exec_command({cmd: \"a\" + \"b\"})",
            // Comment smuggling and unterminated objects.
            "tools.exec_command({cmd: \"a\" /* x */})",
            "tools.exec_command({cmd: \"a\"",
            // No call at all.
            "tools.write_stdin({chars: \"x\"})",
        ] {
            assert!(exec_command_objects(bad).is_empty(), "should reject: {bad}");
        }
        // A parse failure in one call does not hide a later parseable call.
        let objects = exec_command_objects(
            "tools.exec_command({cmd: 'x'}); tools.exec_command({cmd: \"ok\"})",
        );
        assert_eq!(objects.len(), 1);
        assert_eq!(objects[0].get("cmd").and_then(Value::as_str), Some("ok"));
    }

    fn exec_cell_call(turn: &str, js: &str) -> Value {
        json!({
            "timestamp": "t",
            "type": "response_item",
            "payload": {
                "type": "custom_tool_call",
                "id": "ctc_1",
                "status": "completed",
                "call_id": "c1",
                "name": "exec",
                "input": js,
                "internal_chat_message_metadata_passthrough": { "turn_id": turn }
            }
        })
    }

    #[test]
    fn code_mode_exec_cell_provides_owner_call() {
        // The unified-exec code_mode shape observed in real rollouts: the owner call is
        // a custom_tool_call("exec") whose JS drives tools.exec_command.
        let js = "const r = await tools.exec_command({\n  cmd: \"cargo build\",\n  workdir: \"/w\",\n  sandbox_permissions: \"require_escalated\",\n  justification: \"need cargo\",\n  prefix_rule: [\"cargo\"]\n});\ntext(JSON.stringify(r));\n";
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            exec_cell_call("turn-1", js),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        let runner = |probe: &ShellProbe| {
            assert_eq!(
                probe.prefix_rule.as_deref(),
                Some(&["cargo".to_string()][..])
            );
            assert!(probe.sandbox_override, "require_escalated is an override");
            Some(worker_output(&[&["cargo", "build"]]))
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::Enhanced { .. }
        ));

        // A cell whose cmd differs is not the owner: no memory surface.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            exec_cell_call("turn-1", "tools.exec_command({cmd: \"other\"})"),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        let runner = |_probe: &ShellProbe| Some(worker_output(&[&["cargo", "build"]]));
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::Basic
        ));

        // request_permissions mentioned inside a current-turn cell no longer disables
        // memory (D43 relaxed per D50); the exec_command call still provides the owner.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            exec_cell_call(
                "turn-1",
                "await tools.request_permissions({}); tools.exec_command({cmd: \"cargo build\"})",
            ),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::Enhanced { .. }
        ));
    }

    #[test]
    fn explicit_allow_all_returns_auto_allow() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("untrusted"), "restricted"),
            shell_call_full("turn-1", "cargo build", None, None),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        let runner = |_probe: &ShellProbe| {
            let mut output = worker_output(&[&["cargo", "build"]]);
            output.decision = "allow".into();
            output.explicit_allow_all = true;
            Some(output)
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::AutoAllow
        ));
    }

    #[test]
    fn multi_candidate_ladder_flattens_variants_with_recommended_default() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "cargo build --release", None, None),
        ]);
        let input = shell_input(rollout.path(), "cargo build --release");
        let runner = |_probe: &ShellProbe| {
            let mut output = with_amendment(
                worker_output(&[&["cargo", "build", "--release"]]),
                &["cargo", "build", "--release"],
            );
            output.amendment_candidates = vec![
                vec!["cargo".into(), "build".into(), "--release".into()],
                vec!["cargo".into(), "build".into()],
                vec!["cargo".into()],
            ];
            Some(output)
        };
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex_with(&input, false, &runner)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        // Ladder is shortest-first; "build" is subcommand-like so the 2-token candidate
        // (index 1) is recommended and keeps the unsuffixed action ids.
        assert_eq!(
            ids,
            [
                "remember_shell_prefix:2",
                ACTION_SHELL_PREFIX,
                "remember_shell_prefix:0",
                "remember_shell_always:2",
                ACTION_SHELL_ALWAYS,
                "remember_shell_always:0",
                ACTION_SHELL_RELAXED,
            ]
        );
        // Every ladder choice carries selector metadata with aligned levels across groups.
        for (offset, choice) in extra_choices.iter().take(6).enumerate() {
            let variant = choice.variant.as_ref().expect("variant metadata");
            assert_eq!(variant.level, offset % 3);
            assert_eq!(
                variant.group,
                if offset < 3 {
                    "shell-prefix-session"
                } else {
                    "shell-prefix-always"
                }
            );
            assert_eq!(variant.recommended, offset % 3 == 1);
        }
        assert!(extra_choices[6].variant.is_none());
        assert_eq!(
            extra_choices[0].variant.as_ref().unwrap().level_label,
            "cargo"
        );
        assert_eq!(
            extra_choices[1].variant.as_ref().unwrap().level_label,
            "cargo build"
        );
        assert_eq!(
            extra_choices[0].variant.as_ref().unwrap().segment_label,
            "cargo"
        );
        assert_eq!(
            extra_choices[1].variant.as_ref().unwrap().segment_label,
            "build"
        );
        assert_eq!(
            extra_choices[2].variant.as_ref().unwrap().segment_label,
            "--release"
        );
        // Each tier×level has its own save keyed by the action id (+1 relaxed opt-in).
        assert_eq!(memory.saves.len(), 7);
        let session_short = memory
            .saves
            .iter()
            .find(|save| save.action_id == "remember_shell_prefix:2")
            .unwrap();
        assert_eq!(
            session_short.rules,
            vec![RuleKey::ShellPrefix {
                prefix: vec!["cargo".into()]
            }]
        );
        let always_recommended = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_SHELL_ALWAYS)
            .unwrap();
        assert!(matches!(
            always_recommended.native,
            Some(crate::permission_rules::NativeWrite::PrefixRule { ref prefix, .. })
                if prefix == &["cargo".to_string(), "build".into()]
        ));
    }

    #[test]
    fn recommended_candidate_prefers_two_tokens_then_escalates() {
        let ladder = |prefixes: &[&[&str]]| -> Vec<Vec<String>> {
            prefixes
                .iter()
                .map(|prefix| prefix.iter().map(|token| token.to_string()).collect())
                .collect()
        };
        // Subcommand-like second token: 2-token candidate wins.
        let candidates = ladder(&[
            &["cargo", "build", "--release"],
            &["cargo", "build"],
            &["cargo"],
        ]);
        assert_eq!(recommended_candidate_index(&candidates), 1);
        // Flag / path / number second tokens fall back to 1-token.
        for base in [
            &["rg", "-i", "foo"][..],
            &["cat", "/etc/hosts"][..],
            &["kill", "93215"][..],
        ] {
            let candidates = ladder(&[base, &base[..1]]);
            assert_eq!(recommended_candidate_index(&candidates), 1, "{base:?}");
        }
        // Desired tier filtered out (e.g. banned): escalate to the base, never shorter.
        let candidates = ladder(&[&["git", "push", "--force"], &["git", "push"]]);
        assert_eq!(recommended_candidate_index(&candidates), 1);
        let candidates = ladder(&[&["npm", "run", "build"]]);
        assert_eq!(recommended_candidate_index(&candidates), 0);
        // Single-token base recommends itself.
        let candidates = ladder(&[&["ls"]]);
        assert_eq!(recommended_candidate_index(&candidates), 0);
    }

    #[test]
    fn segment_labels_group_tokens_across_filtered_prefix_lengths() {
        let candidates = vec![
            vec!["git".into(), "push".into(), "origin".into(), "main".into()],
            vec!["git".into(), "push".into()],
        ];
        assert_eq!(segment_label(&candidates, 1), "git push");
        assert_eq!(segment_label(&candidates, 0), "origin main");
    }

    #[test]
    fn relaxed_opt_in_gated_by_extended_dangerous_list_and_policy_prompts() {
        // `git push --force` passes the upstream dangerous heuristic (prefix tiers stay)
        // but hits the extended relaxed list: no relaxed opt-in, query not eligible.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "git push --force", None, None),
        ]);
        let input = shell_input(rollout.path(), "git push --force");
        let runner = |_probe: &ShellProbe| {
            Some(with_amendment(
                worker_output(&[&["git", "push", "--force"]]),
                &["git", "push", "--force"],
            ))
        };
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex_with(&input, false, &runner)
        else {
            panic!("expected enhancement");
        };
        assert!(extra_choices
            .iter()
            .all(|choice| choice.id != ACTION_SHELL_RELAXED));
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::ShellCommands {
                relaxed_eligible: false,
                ..
            })
        ));

        // A native prompt-rule hit also disqualifies relaxed eligibility.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "cargo build", None, None),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        let runner = |_probe: &ShellProbe| {
            let mut output =
                with_amendment(worker_output(&[&["cargo", "build"]]), &["cargo", "build"]);
            output.policy_prompt_any = true;
            Some(output)
        };
        let Analysis::Enhanced { memory, .. } = analyze_codex_with(&input, false, &runner) else {
            panic!("expected enhancement");
        };
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::ShellCommands {
                relaxed_eligible: false,
                ..
            })
        ));

        // Plain safe command: eligible, and the opt-in save writes the ShellRelaxed key.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "cargo build", None, None),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        let runner = |_probe: &ShellProbe| {
            Some(with_amendment(
                worker_output(&[&["cargo", "build"]]),
                &["cargo", "build"],
            ))
        };
        let Analysis::Enhanced { memory, .. } = analyze_codex_with(&input, false, &runner) else {
            panic!("expected enhancement");
        };
        assert!(matches!(
            memory.query,
            Some(MemoryQuery::ShellCommands {
                relaxed_eligible: true,
                ..
            })
        ));
        let relaxed = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_SHELL_RELAXED)
            .unwrap();
        assert_eq!(relaxed.rules, vec![RuleKey::ShellRelaxed]);
        assert!(relaxed.native.is_none());
    }

    #[test]
    fn model_prefix_rule_adds_session_prefix_tier() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full(
                "turn-1",
                "cargo build",
                Some(json!(["cargo"])),
                Some("use_default"),
            ),
        ]);
        let input = shell_input(rollout.path(), "cargo build");
        let runner = |probe: &ShellProbe| {
            assert_eq!(
                probe.prefix_rule.as_deref(),
                Some(&["cargo".to_string()][..])
            );
            assert!(!probe.sandbox_override);
            assert_eq!(probe.approval_policy, "on-request");
            assert_eq!(probe.sandbox_kind, "restricted");
            let mut output = with_amendment(worker_output(&[&["cargo", "build"]]), &["cargo"]);
            output.amendment_from_prefix_rule = true;
            Some(output)
        };
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex_with(&input, true, &runner)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        // Prefix-first: the exact tier is not offered when a prefix exists.
        assert_eq!(
            ids,
            [
                ACTION_SHELL_PREFIX,
                ACTION_SHELL_ALWAYS,
                ACTION_SHELL_RELAXED
            ]
        );
        let prefix_save = memory
            .saves
            .iter()
            .find(|save| save.action_id == ACTION_SHELL_PREFIX)
            .unwrap();
        assert_eq!(
            prefix_save.rules,
            vec![RuleKey::ShellPrefix {
                prefix: vec!["cargo".into()]
            }]
        );
        assert!(prefix_save.native.is_none());
    }

    #[test]
    fn dangerous_segments_drop_session_tiers_and_query() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "rm -rf build", None, None),
        ]);
        let input = shell_input(rollout.path(), "rm -rf build");
        let runner = |_probe: &ShellProbe| {
            let mut output = with_amendment(
                worker_output(&[&["rm", "-rf", "build"]]),
                &["rm", "-rf", "build"],
            );
            output.dangerous_any = true;
            Some(output)
        };
        let Analysis::Enhanced {
            memory,
            extra_choices,
        } = analyze_codex_with(&input, false, &runner)
        else {
            panic!("expected enhancement");
        };
        let ids: Vec<&str> = extra_choices
            .iter()
            .map(|choice| choice.id.as_str())
            .collect();
        // Native parity: the permanent amendment option survives, session tiers and the
        // auto-allow query are gated by the dangerous list (D38/D42).
        assert_eq!(ids, [ACTION_SHELL_ALWAYS]);
        assert!(memory.query.is_none());
    }

    #[test]
    fn shell_worker_failure_or_disable_stays_basic() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "git status", None, None),
        ]);
        let input = shell_input(rollout.path(), "git status");
        let none_runner = |_probe: &ShellProbe| None;
        assert!(matches!(
            analyze_codex_with(&input, false, &none_runner),
            Analysis::Basic
        ));
        let disabled_runner = |_probe: &ShellProbe| {
            Some(ShellWorkerOutput {
                disabled_reason: Some("managed overlay".into()),
                ..worker_output(&[])
            })
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &disabled_runner),
            Analysis::Basic
        ));
        let forbidden_runner = |_probe: &ShellProbe| {
            let mut output = worker_output(&[&["git", "status"]]);
            output.decision = "forbidden".into();
            Some(output)
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &forbidden_runner),
            Analysis::Basic
        ));
    }

    #[test]
    fn ownerless_shell_calls_stay_basic() {
        let panicking_runner =
            |_probe: &ShellProbe| -> Option<ShellWorkerOutput> { panic!("must not run worker") };
        // No owner call in the rollout.
        let rollout = write_rollout(&[shell_turn_context(
            "turn-1",
            json!("on-request"),
            "restricted",
        )]);
        let input = shell_input(rollout.path(), "git status");
        assert!(matches!(
            analyze_codex_with(&input, false, &panicking_runner),
            Analysis::Basic
        ));
    }

    #[test]
    fn divergent_owner_fields_merge_conservatively() {
        // D44 refined: the escalated-then-plain retry pattern keeps memory options.
        // Diverging prefix_rule drops the model tier (fallback derivation still runs);
        // any escalated attempt makes the probe escalated.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("untrusted"), "restricted"),
            shell_call_full(
                "turn-1",
                "git status",
                Some(json!(["git"])),
                Some("require_escalated"),
            ),
            shell_call_full("turn-1", "git status", None, None),
        ]);
        let input = shell_input(rollout.path(), "git status");
        let runner = |probe: &ShellProbe| {
            assert_eq!(probe.prefix_rule, None, "diverging model tier is dropped");
            assert!(probe.sandbox_override, "any escalated attempt counts");
            Some(worker_output(&[&["git", "status"]]))
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::Enhanced { .. }
        ));
        // Agreeing owner calls still pass the model tier through.
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("untrusted"), "restricted"),
            shell_call_full("turn-1", "git status", Some(json!(["git"])), None),
            shell_call_full("turn-1", "git status", Some(json!(["git"])), None),
        ]);
        let input = shell_input(rollout.path(), "git status");
        let runner = |probe: &ShellProbe| {
            assert_eq!(probe.prefix_rule.as_deref(), Some(&["git".to_string()][..]));
            assert!(!probe.sandbox_override);
            Some(worker_output(&[&["git", "status"]]))
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::Enhanced { .. }
        ));
    }

    #[test]
    fn real_world_escalated_then_plain_retry_keeps_memory_options() {
        // Byte-level replay of the observed 2026-07-18 session: the model first tries
        // require_escalated (rejected under untrusted, no popup), then retries plain,
        // and the popup arrives with both cells already in the rollout.
        let command = "curl -L --fail --silent --show-error https://example.com | sed -n 's:.*<title>\\([^<]*\\)</title>.*:\\1:p'";
        let escalated = "const r = await tools.exec_command({\n  cmd: \"curl -L --fail --silent --show-error https://example.com | sed -n 's:.*<title>\\\\([^<]*\\\\)</title>.*:\\\\1:p'\",\n  workdir: \"/w\",\n  yield_time_ms: 30000,\n  max_output_tokens: 2000,\n  sandbox_permissions: \"require_escalated\",\n  justification: \"是否允许我联网运行 curl 访问 https://example.com 并提取页面标题？\",\n  prefix_rule: [\"curl\"]\n});\ntext(JSON.stringify(r));\n";
        let plain = "const r = await tools.exec_command({\n  cmd: \"curl -L --fail --silent --show-error https://example.com | sed -n 's:.*<title>\\\\([^<]*\\\\)</title>.*:\\\\1:p'\",\n  workdir: \"/w\",\n  yield_time_ms: 30000,\n  max_output_tokens: 2000\n});\ntext(JSON.stringify(r));\n";
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("untrusted"), "restricted"),
            exec_cell_call("turn-1", escalated),
            exec_cell_call("turn-1", plain),
        ]);
        let input = shell_input(rollout.path(), command);
        let runner = |probe: &ShellProbe| {
            assert_eq!(probe.prefix_rule, None);
            assert!(probe.sandbox_override);
            Some(worker_output(&[
                &[
                    "curl",
                    "-L",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "https://example.com",
                ],
                &["sed", "-n", "s:.*<title>\\([^<]*\\)</title>.*:\\1:p"],
            ]))
        };
        assert!(matches!(
            analyze_codex_with(&input, false, &runner),
            Analysis::Enhanced { .. }
        ));
    }

    #[test]
    fn sandbox_override_flag_reaches_probe() {
        let rollout = write_rollout(&[
            shell_turn_context("turn-1", json!("on-request"), "restricted"),
            shell_call_full("turn-1", "git push", None, Some("require_escalated")),
        ]);
        let input = shell_input(rollout.path(), "git push");
        let runner = |probe: &ShellProbe| {
            assert!(probe.sandbox_override);
            Some(worker_output(&[&["git", "push"]]))
        };
        let Analysis::Enhanced { memory, .. } = analyze_codex_with(&input, false, &runner) else {
            panic!("expected enhancement");
        };
        // No amendment -> the exact session tier plus the relaxed opt-in (D52).
        assert_eq!(memory.saves.len(), 2);
        assert_eq!(memory.saves[0].action_id, ACTION_SHELL_SESSION);
        assert_eq!(memory.saves[1].action_id, ACTION_SHELL_RELAXED);
    }

    #[test]
    fn missing_sandbox_fields_in_turn_context_stay_basic() {
        // The plain turn_context helper carries no sandbox fields: fail closed.
        let rollout = write_rollout(&[
            turn_context("turn-1", Some("user"), json!("on-request")),
            shell_call_full("turn-1", "git status", None, None),
        ]);
        let input = shell_input(rollout.path(), "git status");
        let panicking_runner =
            |_probe: &ShellProbe| -> Option<ShellWorkerOutput> { panic!("must not run worker") };
        assert!(matches!(
            analyze_codex_with(&input, false, &panicking_runner),
            Analysis::Basic
        ));
    }

    #[test]
    fn legacy_sandbox_policy_maps_to_kind() {
        let context = json!({
            "turn_id": "turn-1",
            "approval_policy": "on-request",
            "sandbox_policy": { "type": "danger-full-access" },
        });
        assert_eq!(
            shell_turn_fields(&context),
            Some(("on-request".to_string(), "unrestricted".to_string()))
        );
        let granular = json!({
            "turn_id": "turn-1",
            "approval_policy": { "granular": { "sandbox_approval": true } },
            "file_system_sandbox_policy": { "kind": "unrestricted" },
        });
        assert_eq!(
            shell_turn_fields(&granular),
            Some(("granular".to_string(), "unrestricted".to_string()))
        );
        assert_eq!(shell_turn_fields(&json!({ "turn_id": "turn-1" })), None);
    }

    #[test]
    fn move_patch_records_both_old_and_new_paths() {
        let patch = "*** Begin Patch\n*** Update File: old.txt\n*** Move to: new.txt\n@@\n-x\n+y\n*** End Patch\n";
        let rollout = write_rollout(&[turn_context("turn-1", Some("user"), json!("on-request"))]);
        let Analysis::Enhanced { memory, .. } =
            analyze_codex(&hook_input(rollout.path(), patch), false)
        else {
            panic!("expected enhancement");
        };
        let Some(MemoryQuery::FileEdit { paths }) = memory.query else {
            panic!("expected file query");
        };
        assert_eq!(paths, ["/work/proj/old.txt", "/work/proj/new.txt"]);
    }
}
