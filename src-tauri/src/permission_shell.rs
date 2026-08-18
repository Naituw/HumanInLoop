//! Shell permission analysis worker (spec `docs/specs/codex-permission-remember.md`,
//! D27–D35, D38, D42–D45).
//!
//! The hook process never parses untrusted shell scripts in-process. It spawns
//! `AskHuman __permission-shell-worker` (same isolation pattern as
//! `permission_diff::worker`) with a hard 2s wall clock. The worker:
//!
//! 1. gates on the installed Codex version (D34/D35) and managed-config overlays (D33),
//! 2. splits the `bash -lc` script with the replicated tree-sitter parser (D32),
//! 3. evaluates every segment against the layered `*.rules` files through
//!    `codex execpolicy check` (D31, 500ms per call), falling back to the replicated
//!    heuristics for unmatched segments (D34),
//! 4. reproduces the native amendment derivations for the "always allow" option (D45).
//!
//! Everything here fails closed: any timeout, spawn failure, version drift or managed
//! overlay yields a disabled result and the hook keeps the basic popup.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const WORKER_TIMEOUT_MS: u64 = 2_000;
pub const CHECK_TIMEOUT_MS: u64 = 500;
const VERSION_TIMEOUT_MS: u64 = 1_000;
const MAX_WORKER_STDIN_BYTES: u64 = 1024 * 1024;
const MAX_WORKER_STDOUT_BYTES: u64 = 1024 * 1024;
/// Defensive cap on script segments; longer scripts stay basic.
const MAX_SEGMENTS: usize = 64;

/// Worker stdin payload. The hook assembles it from the PermissionRequest input plus the
/// rollout turn context (approval policy, sandbox kind) and owner FunctionCall (D44).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellWorkerInput {
    pub script: String,
    pub cwd: String,
    pub codex_home: String,
    pub codex_bin: String,
    /// Serde value of the turn's `AskForApproval`: `untrusted` / `on-request` / `never` /
    /// `granular` (object collapsed by the hook).
    pub approval_policy: String,
    /// `FileSystemSandboxKind` kebab-case value; unknown values are treated as restricted.
    pub sandbox_kind: String,
    /// Whether the call requests a sandbox override (`sandbox_permissions != use_default`).
    pub sandbox_override: bool,
    /// Model-provided prefix rule from the owner FunctionCall, if consistent (D44).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix_rule: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellWorkerOutput {
    /// Set when the analysis is unusable (version gate, managed overlay, unsplittable
    /// script, CLI failure). The hook must fall back to the basic popup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    /// Parsed word-only command segments (D32); empty iff disabled.
    #[serde(default)]
    pub segments: Vec<Vec<String>>,
    /// Aggregate policy decision replicating the native evaluation: allow/prompt/forbidden.
    #[serde(default)]
    pub decision: String,
    /// Every segment has an explicit policy rule match with decision allow — the native
    /// `bypass_sandbox` condition and the only auto-allow trigger (D42).
    #[serde(default)]
    pub explicit_allow_all: bool,
    /// Any segment matches the replicated dangerous-command heuristics (D38 gate for
    /// session options and the auto-allow query).
    #[serde(default)]
    pub dangerous_any: bool,
    /// Prefix for the permanent "always allow" option, when native would propose one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amendment: Option<Vec<String>>,
    /// The amendment came from the model's own `prefix_rule` (D38 session prefix tier).
    #[serde(default)]
    pub amendment_from_prefix_rule: bool,
    /// Validated generalizations of `amendment` (D51): the amendment itself first, then
    /// shorter truncations (longest to shortest) that pass the banned-prefix and
    /// dangerous-command checks and would approve every segment (prefix hit or the
    /// segment independently evaluates to allow). Empty iff `amendment` is None.
    #[serde(default)]
    pub amendment_candidates: Vec<Vec<String>>,
    /// Per-segment (parallel to `segments`): the segment independently evaluates to
    /// allow, i.e. it would not itself prompt (all policy matches and the heuristics
    /// fallback agree on allow).
    #[serde(default)]
    pub segment_allows: Vec<bool>,
    /// Any explicit policy rule with decision prompt matched some segment: native
    /// prompts regardless of amendments, and relaxed mode must never auto-allow (D52).
    #[serde(default)]
    pub policy_prompt_any: bool,
}

impl ShellWorkerOutput {
    fn disabled(reason: &str) -> Self {
        Self {
            disabled_reason: Some(reason.to_string()),
            segments: Vec::new(),
            decision: String::new(),
            explicit_allow_all: false,
            dangerous_any: false,
            amendment: None,
            amendment_from_prefix_rule: false,
            amendment_candidates: Vec::new(),
            segment_allows: Vec::new(),
            policy_prompt_any: false,
        }
    }
}

// ===== Policy checker abstraction (real impl shells out to `codex execpolicy check`) =====

pub enum CheckOutcome {
    /// Policy rules matched; the decisions of each matched rule.
    Matched(Vec<String>),
    /// No policy rule matched (CLI printed an empty matchedRules array).
    Unmatched,
    /// A rules file failed to parse. Native falls back to an empty policy with a warning
    /// (`load_exec_policy_with_warning`), so the caller re-evaluates heuristics-only.
    ParseError,
    /// Spawn failure / timeout / unparseable output: fail closed.
    Failed,
}

pub trait PolicyChecker {
    fn codex_version(&self) -> Option<(u32, u32)>;
    fn check(&self, rules: &[PathBuf], segment: &[String]) -> CheckOutcome;
}

pub struct CodexCliChecker {
    pub codex_bin: PathBuf,
}

impl PolicyChecker for CodexCliChecker {
    fn codex_version(&self) -> Option<(u32, u32)> {
        let output = run_process(
            &self.codex_bin,
            &["--version".to_string()],
            None,
            VERSION_TIMEOUT_MS,
        )?;
        if !output.success {
            return None;
        }
        crate::shell_safety::parse_codex_version(&String::from_utf8_lossy(&output.stdout))
    }

    fn check(&self, rules: &[PathBuf], segment: &[String]) -> CheckOutcome {
        let mut args: Vec<String> = vec!["execpolicy".into(), "check".into()];
        for file in rules {
            args.push("--rules".into());
            args.push(file.to_string_lossy().to_string());
        }
        args.push("--resolve-host-executables".into());
        args.push("--".into());
        args.extend(segment.iter().cloned());
        let Some(output) = run_process(&self.codex_bin, &args, None, CHECK_TIMEOUT_MS) else {
            return CheckOutcome::Failed;
        };
        if !output.success {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("failed to parse policy") || stderr.contains("failed to read policy")
            {
                return CheckOutcome::ParseError;
            }
            return CheckOutcome::Failed;
        }
        parse_check_output(&String::from_utf8_lossy(&output.stdout))
    }
}

/// Parses the single-line JSON contract of `codex execpolicy check` (§9.2).
fn parse_check_output(stdout: &str) -> CheckOutcome {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return CheckOutcome::Failed;
    };
    let Some(matched) = value.get("matchedRules").and_then(|rules| rules.as_array()) else {
        return CheckOutcome::Failed;
    };
    if matched.is_empty() {
        return CheckOutcome::Unmatched;
    }
    let mut decisions = Vec::new();
    for rule_match in matched {
        // Externally tagged RuleMatch enum: {"prefixRuleMatch": {..., "decision": "..."}}.
        let Some(decision) = rule_match
            .as_object()
            .and_then(|object| object.values().next())
            .and_then(|inner| inner.get("decision"))
            .and_then(|decision| decision.as_str())
        else {
            return CheckOutcome::Failed;
        };
        decisions.push(decision.to_string());
    }
    CheckOutcome::Matched(decisions)
}

// ===== Worker analysis =====

/// Worker entry point: reads `ShellWorkerInput` JSON from stdin, prints
/// `ShellWorkerOutput` JSON.
pub fn run_stdio() -> Option<String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    std::io::stdin()
        .take(MAX_WORKER_STDIN_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_WORKER_STDIN_BYTES {
        return None;
    }
    let input: ShellWorkerInput = serde_json::from_slice(&bytes).ok()?;
    let checker = CodexCliChecker {
        codex_bin: PathBuf::from(&input.codex_bin),
    };
    let system_config = codex_system_config_dir();
    let output = analyze(&input, &checker, &system_config);
    serde_json::to_string(&output).ok()
}

fn decision_rank(decision: &str) -> u8 {
    match decision {
        "allow" => 0,
        "prompt" => 1,
        _ => 2, // forbidden and anything unknown are maximally restrictive
    }
}

struct SegmentEval {
    /// Decisions of explicit policy matches; empty means no policy rule matched.
    policy_decisions: Vec<String>,
    /// Heuristics fallback decision when unmatched (`render_decision_for_unmatched_command`).
    heuristic: Option<&'static str>,
}

pub fn analyze(
    input: &ShellWorkerInput,
    checker: &dyn PolicyChecker,
    system_config_dir: &Path,
) -> ShellWorkerOutput {
    // Version gate first (D34/D35): floor only. Newer-than-audited versions stay enabled
    // (divergences are conservative on our side and rule writes verify through the user's
    // codex binary) but leave a trail for the periodic upstream sync.
    let Some(version) = checker.codex_version() else {
        return ShellWorkerOutput::disabled("codex version unavailable");
    };
    if !crate::shell_safety::codex_version_supported(version) {
        return ShellWorkerOutput::disabled("codex version below verified floor");
    }
    if crate::shell_safety::codex_version_beyond_verified(version) {
        eprintln!(
            "permission_shell: codex {}.{} is newer than the audited {}.{}; shell memory stays enabled",
            version.0,
            version.1,
            crate::shell_safety::VERIFIED_CODEX_VERSION_CEILING.0,
            crate::shell_safety::VERIFIED_CODEX_VERSION_CEILING.1,
        );
    }

    let codex_home = PathBuf::from(&input.codex_home);
    if let Some(reason) = managed_overlay_reason(&codex_home, system_config_dir) {
        // D33: managed / enterprise overlays change policy semantics we do not replicate.
        return ShellWorkerOutput::disabled(&reason);
    }

    #[cfg(windows)]
    let segments = crate::shell_safety::parse_powershell_plain_commands(&input.script);
    #[cfg(not(windows))]
    let segments = crate::shell_safety::parse_shell_lc_plain_commands(&[
        "bash".to_string(),
        "-lc".to_string(),
        input.script.clone(),
    ]);
    let Some(segments) = segments else {
        return ShellWorkerOutput::disabled("script is not a plain word-only sequence");
    };
    if segments.is_empty() || segments.len() > MAX_SEGMENTS {
        return ShellWorkerOutput::disabled("segment count out of range");
    }

    let mut rules = discover_rules_files(system_config_dir, &codex_home, &input.cwd);

    // Per-segment evaluation; a rules parse error downgrades to the native warning path
    // (empty policy, heuristics only) for all segments.
    let mut evals: Vec<SegmentEval> = Vec::new();
    let mut restart = true;
    while restart {
        restart = false;
        evals.clear();
        for segment in &segments {
            let outcome = if rules.is_empty() {
                CheckOutcome::Unmatched
            } else {
                checker.check(&rules, segment)
            };
            match outcome {
                CheckOutcome::Matched(decisions) => evals.push(SegmentEval {
                    policy_decisions: decisions,
                    heuristic: None,
                }),
                CheckOutcome::Unmatched => evals.push(SegmentEval {
                    policy_decisions: Vec::new(),
                    heuristic: Some(fallback_decision(segment, input)),
                }),
                CheckOutcome::ParseError => {
                    rules.clear();
                    restart = true;
                    break;
                }
                CheckOutcome::Failed => {
                    return ShellWorkerOutput::disabled("execpolicy check failed");
                }
            }
        }
    }

    // Per-segment rank: a segment "independently allows" when every decision that
    // applies to it (policy matches plus the heuristics fallback) is allow.
    let segment_allows: Vec<bool> = evals
        .iter()
        .map(|eval| {
            eval.policy_decisions
                .iter()
                .map(String::as_str)
                .chain(eval.heuristic)
                .map(decision_rank)
                .max()
                .unwrap_or(1)
                == 0
        })
        .collect();

    let decision = evals
        .iter()
        .flat_map(|eval| {
            eval.policy_decisions
                .iter()
                .map(String::as_str)
                .chain(eval.heuristic)
        })
        .max_by_key(|decision| decision_rank(decision))
        .unwrap_or("prompt")
        .to_string();

    let explicit_allow_all = decision == "allow"
        && evals
            .iter()
            .all(|eval| eval.policy_decisions.iter().any(|d| d == "allow"));

    let dangerous_any = segments.iter().any(|segment| segment_is_dangerous(segment));

    let any_policy_match = evals.iter().any(|eval| !eval.policy_decisions.is_empty());
    let policy_prompt = evals
        .iter()
        .any(|eval| eval.policy_decisions.iter().any(|d| d == "prompt"));

    // Amendment derivation, replicating the three native paths (§9.3 / exec_policy.rs).
    let mut amendment: Option<Vec<String>> = None;
    let mut amendment_from_prefix_rule = false;
    if let Some(prefix) = input.prefix_rule.as_ref() {
        // derive_requested_execpolicy_amendment_from_prefix_rule: valid, not banned, no
        // policy match anywhere, and the new rule (plus heuristics fallback) would
        // approve every segment.
        if !prefix.is_empty()
            && !crate::shell_safety::is_banned_prefix(prefix)
            && !any_policy_match
            && segments.iter().zip(&evals).all(|(segment, eval)| {
                let prefix_hits =
                    segment.len() >= prefix.len() && segment[..prefix.len()] == prefix[..];
                prefix_hits || eval.heuristic == Some("allow")
            })
        {
            amendment = Some(prefix.clone());
            amendment_from_prefix_rule = true;
        }
    }
    if amendment.is_none() {
        match decision.as_str() {
            // try_derive_execpolicy_amendment_for_prompt_rules: no policy prompt match,
            // first heuristics Prompt segment.
            "prompt" if !policy_prompt => {
                amendment = segments
                    .iter()
                    .zip(&evals)
                    .find(|(_, eval)| eval.heuristic == Some("prompt"))
                    .map(|(segment, _)| segment.clone());
            }
            // try_derive_execpolicy_amendment_for_allow_rules: no policy match at all,
            // first heuristics Allow segment (native sandbox-retry amendment).
            "allow" if !any_policy_match => {
                amendment = segments
                    .iter()
                    .zip(&evals)
                    .find(|(_, eval)| eval.heuristic == Some("allow"))
                    .map(|(segment, _)| segment.clone());
            }
            _ => {}
        }
    }
    // Banned prefixes are never suggested, whatever the derivation path.
    if amendment
        .as_ref()
        .is_some_and(|prefix| crate::shell_safety::is_banned_prefix(prefix))
    {
        amendment = None;
        amendment_from_prefix_rule = false;
    }

    // Candidate generalizations (D51): the amendment itself plus validated truncations.
    // A truncation qualifies when it is not banned, not itself a dangerous command, and
    // would approve every segment (prefix hit or the segment independently allows).
    let mut amendment_candidates: Vec<Vec<String>> = Vec::new();
    if let Some(base) = amendment.as_ref() {
        amendment_candidates.push(base.clone());
        for len in (1..base.len()).rev() {
            let candidate = &base[..len];
            if crate::shell_safety::is_banned_prefix(candidate) || segment_is_dangerous(candidate) {
                continue;
            }
            let covers_all = segments
                .iter()
                .zip(&segment_allows)
                .all(|(segment, allows)| {
                    (segment.len() >= len && segment[..len] == candidate[..]) || *allows
                });
            if covers_all {
                amendment_candidates.push(candidate.to_vec());
            }
        }
    }

    ShellWorkerOutput {
        disabled_reason: None,
        segments,
        decision,
        explicit_allow_all,
        dangerous_any,
        amendment,
        amendment_from_prefix_rule,
        amendment_candidates,
        segment_allows,
        policy_prompt_any: policy_prompt,
    }
}

/// Replicates `render_decision_for_unmatched_command` (Unix subset, plain parsing only).
fn fallback_decision(segment: &[String], input: &ShellWorkerInput) -> &'static str {
    let dangerous = segment_is_dangerous(segment);
    let known_safe = segment_is_known_safe(segment);

    if known_safe && input.approval_policy == "untrusted" {
        return "allow";
    }
    if dangerous {
        return if input.approval_policy == "never" {
            "forbidden"
        } else {
            "prompt"
        };
    }
    match input.approval_policy.as_str() {
        "never" => "allow",
        "untrusted" => "prompt",
        "on-request" | "granular" => match input.sandbox_kind.as_str() {
            "unrestricted" | "external-sandbox" => "allow",
            // Restricted (and anything unknown, conservatively).
            _ => {
                if input.sandbox_override {
                    "prompt"
                } else {
                    "allow"
                }
            }
        },
        // Unknown policy value from a newer Codex: be conservative.
        _ => "prompt",
    }
}

fn segment_is_dangerous(segment: &[String]) -> bool {
    #[cfg(windows)]
    {
        crate::shell_safety::is_dangerous_powershell_words(segment)
    }
    #[cfg(not(windows))]
    {
        crate::shell_safety::is_dangerous_command(segment)
    }
}

fn segment_is_known_safe(segment: &[String]) -> bool {
    #[cfg(windows)]
    {
        crate::shell_safety::is_safe_powershell_words(segment)
    }
    #[cfg(not(windows))]
    {
        crate::shell_safety::is_known_safe_command(segment)
    }
}

// ===== Config layer discovery (D33-lite) =====

/// Managed / enterprise overlays we do not replicate: presence disables shell memory.
fn managed_overlay_reason(codex_home: &Path, system_config_dir: &Path) -> Option<String> {
    for name in ["requirements.toml", "managed_config.toml"] {
        if system_config_dir.join(name).exists() {
            return Some(format!("system {name} present"));
        }
    }
    if codex_home.join("cloud-config-bundle-cache.json").exists() {
        return Some("cloud config bundle cache present".to_string());
    }
    #[cfg(target_os = "macos")]
    {
        let prefs = Path::new("/Library/Managed Preferences");
        if prefs.join("com.openai.codex.plist").exists() {
            return Some("macOS managed preferences present".to_string());
        }
        if let Ok(entries) = std::fs::read_dir(prefs) {
            for entry in entries.flatten() {
                if entry.path().join("com.openai.codex.plist").exists() {
                    return Some("macOS managed preferences present".to_string());
                }
            }
        }
    }
    None
}

/// `*.rules` files of the readable config layers, lowest precedence first:
/// system → user → trusted project layers (outermost first). Mirrors
/// `load_exec_policy` with untrusted project layers excluded (disabled layers).
fn discover_rules_files(system_config_dir: &Path, codex_home: &Path, cwd: &str) -> Vec<PathBuf> {
    let mut files = Vec::new();
    push_rules_dir(&mut files, &system_config_dir.join("rules"));
    push_rules_dir(&mut files, &codex_home.join("rules"));

    if let Some(normalized_cwd) = crate::permission_rules::normalize_path(".", cwd) {
        let cwd_path = PathBuf::from(&normalized_cwd);
        let stop = crate::project::git_root(&cwd_path).unwrap_or_else(|| cwd_path.clone());
        let mut chain: Vec<&Path> = Vec::new();
        let mut dir = Some(cwd_path.as_path());
        while let Some(current) = dir {
            chain.push(current);
            if current == stop {
                break;
            }
            dir = current.parent();
        }
        // Outermost project layer first (lowest precedence among project layers).
        for current in chain.into_iter().rev() {
            if current.join(".codex").is_dir()
                && crate::permission_memory::codex_project_trusted(codex_home, current)
            {
                push_rules_dir(&mut files, &current.join(".codex/rules"));
            }
        }
    }
    files
}

fn push_rules_dir(files: &mut Vec<PathBuf>, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("rules") && path.is_file()
        })
        .collect();
    found.sort();
    files.extend(found);
}

// ===== Hook-side orchestration =====

/// Locates the Codex binary: the hook's parent process (Codex spawned us) when its
/// basename mentions codex, otherwise a PATH lookup (D31).
pub fn locate_codex_bin() -> Option<PathBuf> {
    if let Some(parent) = parent_process_exe() {
        let basename = parent
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        // Case-insensitive: the desktop app's process is "Codex", the CLI is "codex".
        if basename.to_ascii_lowercase().contains("codex") {
            return Some(parent);
        }
    }
    which_codex()
}

#[cfg(target_os = "macos")]
fn parent_process_exe() -> Option<PathBuf> {
    let ppid = unsafe { libc::getppid() };
    let mut buffer = vec![0u8; 4096];
    let written = unsafe {
        libc::proc_pidpath(
            ppid,
            buffer.as_mut_ptr() as *mut std::ffi::c_void,
            buffer.len() as u32,
        )
    };
    if written <= 0 {
        return None;
    }
    let path = String::from_utf8_lossy(&buffer[..written as usize]).to_string();
    Some(PathBuf::from(path))
}

#[cfg(target_os = "linux")]
fn parent_process_exe() -> Option<PathBuf> {
    let ppid = unsafe { libc::getppid() };
    std::fs::read_link(format!("/proc/{ppid}/exe")).ok()
}

#[cfg(windows)]
fn parent_process_exe() -> Option<PathBuf> {
    let mut pid = std::process::id();
    for _ in 0..12 {
        let identity = crate::agents::detect::inspect_process(pid)?;
        pid = identity.parent_pid;
        let executable = identity.executable.map(PathBuf::from);
        if executable.as_ref().is_some_and(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.to_ascii_lowercase().contains("codex"))
        }) {
            return executable;
        }
        if pid == 0 {
            break;
        }
    }
    None
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn parent_process_exe() -> Option<PathBuf> {
    None
}

fn which_codex() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            #[cfg(windows)]
            candidates.push(dir.join("codex.exe"));
            candidates.push(dir.join("codex"));
        }
    }
    // Daemon contexts (launchd/systemd) may run with a minimal PATH.
    #[cfg(target_os = "macos")]
    candidates.push(PathBuf::from("/opt/homebrew/bin/codex"));
    #[cfg(unix)]
    candidates.push(PathBuf::from("/usr/local/bin/codex"));
    #[cfg(windows)]
    if let Some(app_data) = std::env::var_os("APPDATA") {
        let npm = PathBuf::from(app_data).join("npm/node_modules/@openai/codex/node_modules");
        candidates.push(
            npm.join("@openai/codex-win32-x64/vendor/x86_64-pc-windows-msvc/codex/codex.exe"),
        );
        candidates.push(
            npm.join("@openai/codex-win32-arm64/vendor/aarch64-pc-windows-msvc/codex/codex.exe"),
        );
    }
    candidates.into_iter().find(|path| is_executable(path))
}

#[cfg(windows)]
fn codex_system_config_dir() -> PathBuf {
    std::env::var_os("ProgramData")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
        .join("OpenAI/Codex")
}

#[cfg(not(windows))]
fn codex_system_config_dir() -> PathBuf {
    PathBuf::from("/etc/codex")
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Hook-side request: everything the hook derived from stdin + rollout (D44).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProbe {
    pub script: String,
    pub cwd: String,
    pub approval_policy: String,
    pub sandbox_kind: String,
    pub sandbox_override: bool,
    pub prefix_rule: Option<Vec<String>>,
}

/// Spawns the isolated worker and returns its output. `None` on any failure.
pub fn analyze_for_hook(probe: &ShellProbe) -> Option<ShellWorkerOutput> {
    let codex_home = crate::permission_memory::default_codex_home()?;
    let codex_bin = locate_codex_bin()?;
    let input = ShellWorkerInput {
        script: probe.script.clone(),
        cwd: probe.cwd.clone(),
        codex_home: codex_home.to_string_lossy().to_string(),
        codex_bin: codex_bin.to_string_lossy().to_string(),
        approval_policy: probe.approval_policy.clone(),
        sandbox_kind: probe.sandbox_kind.clone(),
        sandbox_override: probe.sandbox_override,
        prefix_rule: probe.prefix_rule.clone(),
    };
    let payload = serde_json::to_vec(&input).ok()?;
    if payload.len() as u64 > MAX_WORKER_STDIN_BYTES {
        return None;
    }
    let binary = std::env::current_exe().ok()?;
    // Worker budget (D45) plus spawn margin.
    let output = run_process_with_args(
        &binary,
        &["__permission-shell-worker".to_string()],
        Some(&payload),
        WORKER_TIMEOUT_MS + 500,
    )?;
    if !output.success || output.stdout.len() as u64 > MAX_WORKER_STDOUT_BYTES {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

// ===== Daemon-side amendment verification (D45) =====

/// Verifies a prefix rule with the installed Codex before it is written to
/// `default.rules`: the serialized line must parse and yield an allow decision for the
/// prefix itself. When no Codex binary can be found the verification is skipped
/// (structural validation already happened); a failing verification is fatal (D25).
pub fn verify_prefix_rule(prefix: &[String]) -> Result<(), String> {
    if prefix.is_empty() {
        return Err("empty prefix".to_string());
    }
    if crate::shell_safety::is_banned_prefix(prefix) {
        return Err("banned prefix".to_string());
    }
    let Some(codex_bin) = locate_codex_bin() else {
        return Ok(());
    };
    let tokens = prefix
        .iter()
        .map(|token| serde_json::to_string(token).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?;
    let line = format!(
        r#"prefix_rule(pattern=[{}], decision="allow")"#,
        tokens.join(", ")
    );
    let dir = std::env::temp_dir().join(format!("ah-execpolicy-verify-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|error| format!("create verify dir: {error}"))?;
    let file = dir.join("verify.rules");
    let result = (|| {
        std::fs::write(&file, format!("{line}\n")).map_err(|error| format!("write: {error}"))?;
        let checker = CodexCliChecker { codex_bin };
        match checker.check(std::slice::from_ref(&file), prefix) {
            CheckOutcome::Matched(decisions) if decisions.iter().any(|d| d == "allow") => Ok(()),
            CheckOutcome::Matched(_) | CheckOutcome::Unmatched => {
                Err("verification rule did not allow the prefix".to_string())
            }
            CheckOutcome::ParseError => Err("verification rule failed to parse".to_string()),
            CheckOutcome::Failed => Err("verification run failed".to_string()),
        }
    })();
    let _ = std::fs::remove_dir_all(&dir);
    result
}

// ===== Minimal synchronous subprocess runner with a hard timeout =====

struct ProcessOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

fn run_process(
    binary: &Path,
    args: &[String],
    stdin_payload: Option<&[u8]>,
    timeout_ms: u64,
) -> Option<ProcessOutput> {
    run_process_with_args(binary, args, stdin_payload, timeout_ms)
}

fn run_process_with_args(
    binary: &Path,
    args: &[String],
    stdin_payload: Option<&[u8]>,
    timeout_ms: u64,
) -> Option<ProcessOutput> {
    use std::io::{Read, Write};
    use std::process::{Command, Stdio};

    let mut child = Command::new(binary)
        .args(args)
        .stdin(if stdin_payload.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;

    if let Some(payload) = stdin_payload {
        let mut stdin = child.stdin.take()?;
        let payload = payload.to_vec();
        // Writer thread so a stalled child cannot block us past the deadline.
        std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        });
    }

    let mut stdout_pipe = child.stdout.take()?;
    let mut stderr_pipe = child.stderr.take()?;
    let stdout_task = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stdout_pipe
            .by_ref()
            .take(MAX_WORKER_STDOUT_BYTES + 1)
            .read_to_end(&mut buffer);
        buffer
    });
    let stderr_task = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = stderr_pipe
            .by_ref()
            .take(64 * 1024)
            .read_to_end(&mut buffer);
        buffer
    });

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };

    let stdout = stdout_task.join().ok()?;
    let stderr = stderr_task.join().ok()?;
    Some(ProcessOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// Canned policy: maps a segment (joined by \x1f) to an outcome.
    struct FakeChecker {
        version: Option<(u32, u32)>,
        outcomes: HashMap<String, Vec<String>>,
        parse_error: bool,
        calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeChecker {
        fn new(version: Option<(u32, u32)>) -> Self {
            Self {
                version,
                outcomes: HashMap::new(),
                parse_error: false,
                calls: RefCell::new(Vec::new()),
            }
        }
        fn with_rule(mut self, segment: &[&str], decisions: &[&str]) -> Self {
            self.outcomes.insert(
                segment.join("\x1f"),
                decisions.iter().map(|d| d.to_string()).collect(),
            );
            self
        }
    }

    impl PolicyChecker for FakeChecker {
        fn codex_version(&self) -> Option<(u32, u32)> {
            self.version
        }
        fn check(&self, _rules: &[PathBuf], segment: &[String]) -> CheckOutcome {
            self.calls.borrow_mut().push(segment.to_vec());
            if self.parse_error {
                return CheckOutcome::ParseError;
            }
            match self.outcomes.get(&segment.join("\x1f")) {
                Some(decisions) => CheckOutcome::Matched(decisions.clone()),
                None => CheckOutcome::Unmatched,
            }
        }
    }

    struct Env {
        home: tempfile::TempDir,
        system: tempfile::TempDir,
    }

    impl Env {
        fn new() -> Self {
            let env = Self {
                home: tempfile::tempdir().unwrap(),
                system: tempfile::tempdir().unwrap(),
            };
            // A rules file must exist for the checker to be consulted at all.
            std::fs::create_dir_all(env.home.path().join("rules")).unwrap();
            std::fs::write(env.home.path().join("rules/default.rules"), "").unwrap();
            env
        }
        fn input(&self, script: &str) -> ShellWorkerInput {
            ShellWorkerInput {
                script: script.to_string(),
                cwd: "/work/proj".to_string(),
                codex_home: self.home.path().to_string_lossy().to_string(),
                codex_bin: "/usr/bin/false".to_string(),
                approval_policy: "on-request".to_string(),
                sandbox_kind: "restricted".to_string(),
                sandbox_override: true,
                prefix_rule: None,
            }
        }
    }

    #[test]
    fn version_gate_disables_analysis() {
        let env = Env::new();
        // Below the floor (pre-hook versions) or unknown: disabled.
        let checker = FakeChecker::new(Some((0, 121)));
        let output = analyze(&env.input("ls"), &checker, env.system.path());
        assert!(output.disabled_reason.is_some());
        let checker = FakeChecker::new(None);
        let output = analyze(&env.input("ls"), &checker, env.system.path());
        assert!(output.disabled_reason.is_some());
        // Newer than the audited ceiling: stays enabled (no ceiling, log only).
        let checker = FakeChecker::new(Some((99, 0)));
        let output = analyze(&env.input("ls"), &checker, env.system.path());
        assert!(output.disabled_reason.is_none());
    }

    #[test]
    fn managed_overlay_disables_analysis() {
        let env = Env::new();
        std::fs::create_dir_all(env.system.path()).unwrap();
        std::fs::write(env.system.path().join("requirements.toml"), "").unwrap();
        let checker = FakeChecker::new(Some((0, 144)));
        let output = analyze(&env.input("ls"), &checker, env.system.path());
        assert!(output
            .disabled_reason
            .as_deref()
            .unwrap()
            .contains("requirements.toml"));

        let env2 = Env::new();
        std::fs::write(
            env2.home.path().join("cloud-config-bundle-cache.json"),
            "{}",
        )
        .unwrap();
        let output = analyze(
            &env2.input("ls"),
            &FakeChecker::new(Some((0, 144))),
            env2.system.path(),
        );
        assert!(output.disabled_reason.is_some());
    }

    #[test]
    fn unsplittable_scripts_are_disabled() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));
        for script in ["echo $(pwd)", "ls > out.txt", "FOO=bar ls"] {
            let output = analyze(&env.input(script), &checker, env.system.path());
            assert!(output.disabled_reason.is_some(), "{script}");
        }
    }

    #[test]
    fn explicit_allow_all_requires_policy_match_on_every_segment() {
        let env = Env::new();
        // Both segments explicitly allowed.
        let checker = FakeChecker::new(Some((0, 144)))
            .with_rule(&["cargo", "build"], &["allow"])
            .with_rule(&["cargo", "test"], &["allow"]);
        let output = analyze(
            &env.input("cargo build && cargo test"),
            &checker,
            env.system.path(),
        );
        assert_eq!(output.disabled_reason, None);
        assert_eq!(output.decision, "allow");
        assert!(output.explicit_allow_all);

        // Second segment only heuristics-allowed (ls with no override would be allow, but
        // here override=true -> prompt); mixed case must not claim explicit allow.
        let checker = FakeChecker::new(Some((0, 144))).with_rule(&["cargo", "build"], &["allow"]);
        let mut input = env.input("cargo build && ls");
        input.sandbox_override = false;
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.decision, "allow");
        assert!(
            !output.explicit_allow_all,
            "heuristics never prove explicit allow (D42)"
        );
    }

    #[test]
    fn prompt_decision_and_heuristic_amendment() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));
        // cargo build unmatched -> heuristic prompt (restricted + override).
        let output = analyze(&env.input("cargo build"), &checker, env.system.path());
        assert_eq!(output.decision, "prompt");
        assert!(!output.explicit_allow_all);
        assert_eq!(output.amendment, Some(vec!["cargo".into(), "build".into()]));
        assert!(!output.amendment_from_prefix_rule);
    }

    #[test]
    fn policy_prompt_match_blocks_amendment() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144))).with_rule(&["cargo", "build"], &["prompt"]);
        let output = analyze(&env.input("cargo build"), &checker, env.system.path());
        assert_eq!(output.decision, "prompt");
        assert_eq!(output.amendment, None);
        assert!(output.policy_prompt_any);
        assert!(output.amendment_candidates.is_empty());
    }

    #[test]
    fn amendment_candidates_list_validated_truncations() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));

        // Single segment: every truncation is a prefix hit -> full ladder.
        let output = analyze(
            &env.input("cargo build --release"),
            &checker,
            env.system.path(),
        );
        let full: Vec<String> = vec!["cargo".into(), "build".into(), "--release".into()];
        assert_eq!(output.amendment, Some(full.clone()));
        assert_eq!(
            output.amendment_candidates,
            vec![
                full,
                vec!["cargo".to_string(), "build".to_string()],
                vec!["cargo".to_string()],
            ]
        );
        assert!(!output.policy_prompt_any);

        // Banned truncations are skipped (["git"] is banned since 0.145).
        let output = analyze(
            &env.input("git push origin main"),
            &checker,
            env.system.path(),
        );
        assert_eq!(
            output.amendment_candidates,
            vec![
                vec![
                    "git".to_string(),
                    "push".to_string(),
                    "origin".to_string(),
                    "main".to_string()
                ],
                vec!["git".to_string(), "push".to_string(), "origin".to_string()],
                vec!["git".to_string(), "push".to_string()],
            ]
        );

        // Multi-segment: a truncation must cover every segment (prefix hit or the
        // segment allows on its own).
        let output = analyze(
            &env.input("cargo build && cargo test"),
            &checker,
            env.system.path(),
        );
        assert_eq!(output.amendment, Some(vec!["cargo".into(), "build".into()]));
        assert_eq!(
            output.amendment_candidates,
            vec![
                vec!["cargo".to_string(), "build".to_string()],
                vec!["cargo".to_string()],
            ]
        );

        // ls under override prompts on its own and is not covered by ["cargo"]:
        // only the base amendment survives.
        let output = analyze(&env.input("cargo build && ls"), &checker, env.system.path());
        assert_eq!(output.segment_allows, vec![false, false]);
        assert_eq!(
            output.amendment_candidates,
            vec![vec!["cargo".to_string(), "build".to_string()]]
        );

        // Under untrusted policy ls is known-safe (allows on its own), so ["cargo"]
        // only needs to cover the cargo segment.
        let mut input = env.input("cargo build && ls");
        input.approval_policy = "untrusted".to_string();
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.segment_allows, vec![false, true]);
        assert_eq!(
            output.amendment_candidates,
            vec![
                vec!["cargo".to_string(), "build".to_string()],
                vec!["cargo".to_string()],
            ]
        );
    }

    #[test]
    fn amendment_candidates_skip_dangerous_truncations() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));
        let output = analyze(&env.input("rm -rf /tmp/x"), &checker, env.system.path());
        // Base amendment is the prompting segment itself; ["rm"] is banned and
        // ["rm", "-rf"] is dangerous, so no generalization is offered.
        assert_eq!(
            output.amendment,
            Some(vec!["rm".into(), "-rf".into(), "/tmp/x".into()])
        );
        assert_eq!(
            output.amendment_candidates,
            vec![vec![
                "rm".to_string(),
                "-rf".to_string(),
                "/tmp/x".to_string()
            ]]
        );
    }

    #[test]
    fn requested_prefix_rule_amendment() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));
        let mut input = env.input("cargo build && ls");
        // ls is heuristics-prompt under override, so the prefix alone cannot cover it:
        // the requested amendment is rejected and the derivation falls back to the first
        // prompting segment (native behavior).
        input.prefix_rule = Some(vec!["cargo".into()]);
        let output = analyze(&input, &checker, env.system.path());
        assert!(
            !output.amendment_from_prefix_rule,
            "prefix must approve every segment"
        );
        assert_eq!(output.amendment, Some(vec!["cargo".into(), "build".into()]));

        // Without the override, ls falls back to allow and the prefix covers the rest.
        let mut input = env.input("cargo build && ls");
        input.sandbox_override = false;
        input.prefix_rule = Some(vec!["cargo".into()]);
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.decision, "allow");
        assert_eq!(output.amendment, Some(vec!["cargo".into()]));
        assert!(output.amendment_from_prefix_rule);

        // Banned prefixes are rejected outright.
        let mut input = env.input("git push origin main");
        input.prefix_rule = Some(vec!["git".into()]);
        let output = analyze(&input, &checker, env.system.path());
        assert!(!output.amendment_from_prefix_rule);
        // The fallback derivation still proposes the prompting segment itself.
        assert_eq!(
            output.amendment,
            Some(vec![
                "git".into(),
                "push".into(),
                "origin".into(),
                "main".into()
            ])
        );
    }

    #[test]
    fn dangerous_segments_are_flagged_and_never_amended_via_prefix() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));
        let output = analyze(&env.input("rm -rf /tmp/x"), &checker, env.system.path());
        assert_eq!(output.decision, "prompt");
        assert!(output.dangerous_any);

        // never policy forbids dangerous commands.
        let mut input = env.input("rm -rf /tmp/x");
        input.approval_policy = "never".to_string();
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.decision, "forbidden");
    }

    #[test]
    fn parse_error_downgrades_to_heuristics_only() {
        let env = Env::new();
        let mut checker = FakeChecker::new(Some((0, 144)));
        checker.parse_error = true;
        let mut input = env.input("ls");
        input.sandbox_override = false;
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.disabled_reason, None);
        assert_eq!(output.decision, "allow");
        assert!(!output.explicit_allow_all);
    }

    #[test]
    fn untrusted_policy_allows_known_safe_and_prompts_others() {
        let env = Env::new();
        let checker = FakeChecker::new(Some((0, 144)));
        let mut input = env.input("ls && pwd");
        input.approval_policy = "untrusted".to_string();
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.decision, "allow");

        let mut input = env.input("cargo build");
        input.approval_policy = "untrusted".to_string();
        let output = analyze(&input, &checker, env.system.path());
        assert_eq!(output.decision, "prompt");
    }

    #[test]
    fn check_output_parsing_matches_cli_contract() {
        match parse_check_output(
            r#"{"matchedRules":[{"prefixRuleMatch":{"matchedPrefix":["cargo"],"decision":"allow"}}],"decision":"allow"}"#,
        ) {
            CheckOutcome::Matched(decisions) => assert_eq!(decisions, ["allow"]),
            _ => panic!("expected matched"),
        }
        assert!(matches!(
            parse_check_output(r#"{"matchedRules":[]}"#),
            CheckOutcome::Unmatched
        ));
        assert!(matches!(parse_check_output("junk"), CheckOutcome::Failed));
    }

    #[test]
    fn rules_discovery_orders_layers_and_skips_untrusted_projects() {
        let home = tempfile::tempdir().unwrap();
        let system = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(system.path().join("rules")).unwrap();
        std::fs::write(system.path().join("rules/sys.rules"), "").unwrap();
        std::fs::create_dir_all(home.path().join("rules")).unwrap();
        std::fs::write(home.path().join("rules/b.rules"), "").unwrap();
        std::fs::write(home.path().join("rules/a.rules"), "").unwrap();
        std::fs::write(home.path().join("rules/notes.txt"), "").unwrap();

        let project = tempfile::tempdir().unwrap();
        let root = project.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join(".codex/rules")).unwrap();
        std::fs::write(root.join(".codex/rules/proj.rules"), "").unwrap();

        let cwd = root.to_string_lossy().to_string();
        // Untrusted project: its rules are excluded (disabled layer).
        let files = discover_rules_files(system.path(), home.path(), &cwd);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["sys.rules", "a.rules", "b.rules"]);

        // Trusted project layer joins after the user layer.
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[projects.{}]\ntrust_level = \"trusted\"\n",
                toml_edit::Value::from(root.to_string_lossy().as_ref())
            ),
        )
        .unwrap();
        let files = discover_rules_files(system.path(), home.path(), &cwd);
        let names: Vec<String> = files
            .iter()
            .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["sys.rules", "a.rules", "b.rules", "proj.rules"]);
    }

    /// End-to-end against the real installed codex binary; skipped when the local codex
    /// is missing or outside the verified window (D35).
    #[test]
    fn real_codex_cli_contract_when_available() {
        let Some(codex_bin) = which_codex() else {
            return;
        };
        let checker = CodexCliChecker { codex_bin };
        let Some(version) = checker.codex_version() else {
            return;
        };
        if !crate::shell_safety::codex_version_supported(version) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let rules = dir.path().join("probe.rules");
        std::fs::write(
            &rules,
            "prefix_rule(pattern=[\"cargo\"], decision=\"allow\")\nprefix_rule(pattern=[\"git\", \"push\"], decision=\"prompt\")\n",
        )
        .unwrap();
        match checker.check(
            std::slice::from_ref(&rules),
            &["cargo".into(), "build".into()],
        ) {
            CheckOutcome::Matched(decisions) => assert_eq!(decisions, ["allow"]),
            _ => panic!("expected allow match"),
        }
        assert!(matches!(
            checker.check(std::slice::from_ref(&rules), &["ls".into()]),
            CheckOutcome::Unmatched
        ));
        match checker.check(
            std::slice::from_ref(&rules),
            &["git".into(), "push".into(), "x".into()],
        ) {
            CheckOutcome::Matched(decisions) => assert_eq!(decisions, ["prompt"]),
            _ => panic!("expected prompt match"),
        }
        let broken = dir.path().join("broken.rules");
        std::fs::write(&broken, "bogus(\n").unwrap();
        assert!(matches!(
            checker.check(&[broken], &["ls".into()]),
            CheckOutcome::ParseError
        ));

        // D45 verification round-trip with the real binary.
        assert!(verify_prefix_rule(&["cargo".into(), "build".into()]).is_ok());
        assert!(verify_prefix_rule(&["git".into()]).is_err());
    }
}
