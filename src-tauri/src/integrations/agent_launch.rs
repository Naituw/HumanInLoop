//! Secure Terminal launch bridge for tasks created from IM.
//!
//! IM data is stored in a private one-time record. AppleScript and the login shell only receive
//! the absolute AskHuman executable plus an opaque UUID token.

use crate::agents::AgentKind;
use crate::config::AgentTaskPermission;
use crate::integrations::agent_rules::{self, AgentTarget};
use crate::integrations::mcp_config;
use crate::integrations::{agent_lifecycle, agent_mode};
use crate::paths;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
#[cfg(test)]
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
#[cfg(test)]
use std::thread::ThreadId;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RECORD_TTL_SECS: u64 = 5 * 60;
const MAX_TASK_CHARS: usize = 3000;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
const PROBE_CACHE_TTL: Duration = Duration::from_secs(60);
pub const LAUNCH_ID_ENV: &str = "ASKHUMAN_AGENT_TASK_LAUNCH_ID";
pub const PI_MINIMUM_VERSION: &str = "0.82.0";
const PI_MINIMUM: (u64, u64, u64) = (0, 82, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LaunchPermission {
    AgentDefault,
    Yolo,
}

/// The one-time launch protocol is shared by fresh tasks and native session forks. Missing fields
/// in records written by older AskHuman versions deserialize as `New`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum LaunchMode {
    #[default]
    New,
    Fork {
        source_session_id: String,
    },
}

impl TryFrom<AgentTaskPermission> for LaunchPermission {
    type Error = anyhow::Error;

    fn try_from(value: AgentTaskPermission) -> Result<Self> {
        match value {
            AgentTaskPermission::AgentDefault => Ok(Self::AgentDefault),
            AgentTaskPermission::Yolo => Ok(Self::Yolo),
            AgentTaskPermission::Ask => Err(anyhow!("permission choice is still required")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchSource {
    pub channel: String,
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRecord {
    pub id: String,
    pub created_at: u64,
    pub expires_at: u64,
    pub source: LaunchSource,
    pub task: String,
    pub task_sha256: String,
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub payload_sha256: String,
    pub cwd: String,
    pub kind: AgentKind,
    pub permission: LaunchPermission,
    pub executable: String,
    pub askhuman_executable: String,
    #[serde(default)]
    pub launch_mode: LaunchMode,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkReadiness {
    pub kind: AgentKind,
    pub ready: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReadiness {
    pub kind: AgentKind,
    pub label: String,
    pub command: String,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub binary_ready: bool,
    pub lifecycle_ready: bool,
    pub integration_ready: bool,
    pub integration_mode: String,
    pub ready: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct BinaryProbe {
    executable: Option<String>,
    pi_version: Option<(u64, u64, u64)>,
}

struct ProbeCache {
    values: HashMap<AgentKind, (Instant, BinaryProbe)>,
    inflight: HashMap<AgentKind, Arc<Mutex<()>>>,
}

fn probe_cache() -> &'static Mutex<ProbeCache> {
    static CACHE: OnceLock<Mutex<ProbeCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(ProbeCache {
            values: HashMap::new(),
            inflight: HashMap::new(),
        })
    })
}

#[cfg(test)]
struct BinaryProbeTestHarness {
    lock: Mutex<()>,
    threads: Mutex<HashSet<ThreadId>>,
    r#override: Mutex<Option<BinaryProbe>>,
    hits: AtomicUsize,
}

#[cfg(test)]
fn binary_probe_test_harness() -> &'static BinaryProbeTestHarness {
    static HARNESS: OnceLock<BinaryProbeTestHarness> = OnceLock::new();
    HARNESS.get_or_init(|| BinaryProbeTestHarness {
        lock: Mutex::new(()),
        threads: Mutex::new(HashSet::new()),
        r#override: Mutex::new(None),
        hits: AtomicUsize::new(0),
    })
}

pub fn readiness(kind: AgentKind) -> AgentReadiness {
    readiness_with(kind, false)
}

pub fn readiness_fresh(kind: AgentKind) -> AgentReadiness {
    readiness_with(kind, true)
}

fn readiness_with(kind: AgentKind, force: bool) -> AgentReadiness {
    let command = command_name(kind).to_string();
    let probe = binary_probe(kind, force);
    let executable = probe.executable.clone();
    let lifecycle = agent_lifecycle::status(kind);
    let target = target(kind);
    let mode = agent_mode::current(target);
    let integration_ready = !integration_unavailable(target, mode);
    let pi_version = (kind == AgentKind::Pi)
        .then_some(probe.pi_version)
        .flatten();
    let version_ready = kind != AgentKind::Pi || pi_version.is_some_and(pi_version_supported);
    let binary_ready = executable.is_some() && version_ready;
    let lifecycle_ready =
        lifecycle.supported && lifecycle.enabled && lifecycle.installed && !lifecycle.outdated;
    let mut diagnostics = Vec::new();
    if !binary_ready {
        if executable.is_none() {
            diagnostics.push(format!(
                "{} CLI was not found in the login shell",
                kind.label()
            ));
        } else if kind == AgentKind::Pi {
            diagnostics.push(match pi_version {
                Some((major, minor, patch)) => format!(
                    "Pi {major}.{minor}.{patch} is unsupported; AskHuman requires Pi >= {PI_MINIMUM_VERSION}"
                ),
                None => {
                    format!(
                        "Pi version could not be detected; AskHuman requires Pi >= {PI_MINIMUM_VERSION}"
                    )
                }
            });
        }
    }
    if !lifecycle_ready {
        diagnostics.push(format!(
            "{} lifecycle tracking is missing or outdated",
            kind.label()
        ));
    }
    if !integration_ready {
        diagnostics.push(format!(
            "{} AskHuman integration is disabled or unavailable",
            kind.label()
        ));
    }
    AgentReadiness {
        kind,
        label: kind.label().to_string(),
        command,
        executable,
        version: pi_version.map(|(major, minor, patch)| format!("{major}.{minor}.{patch}")),
        binary_ready,
        lifecycle_ready,
        integration_ready,
        integration_mode: mode.as_str().to_string(),
        ready: binary_ready && lifecycle_ready && integration_ready,
        diagnostics,
    }
}

fn binary_probe(kind: AgentKind, force: bool) -> BinaryProbe {
    if !force {
        if let Some(hit) = cached_probe(kind) {
            return hit;
        }
    }
    let gate = {
        let mut cache = probe_cache().lock().unwrap();
        cache
            .inflight
            .entry(kind)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = gate.lock().unwrap();
    if !force {
        if let Some(hit) = cached_probe(kind) {
            return hit;
        }
    }
    let value = binary_probe_uncached(kind);
    probe_cache()
        .lock()
        .unwrap()
        .values
        .insert(kind, (Instant::now(), value.clone()));
    value
}

fn cached_probe(kind: AgentKind) -> Option<BinaryProbe> {
    let cache = probe_cache().lock().unwrap();
    let (at, value) = cache.values.get(&kind)?;
    (at.elapsed() < PROBE_CACHE_TTL).then(|| value.clone())
}

fn binary_probe_uncached(kind: AgentKind) -> BinaryProbe {
    #[cfg(test)]
    {
        let harness = binary_probe_test_harness();
        let participating = harness
            .threads
            .lock()
            .unwrap()
            .contains(&std::thread::current().id());
        if participating {
            harness.hits.fetch_add(1, Ordering::SeqCst);
            if let Some(value) = harness.r#override.lock().unwrap().clone() {
                return value;
            }
        }
    }
    let executable = resolve_login_shell_executable(command_name(kind));
    let pi_version = (kind == AgentKind::Pi)
        .then(|| executable.as_deref().and_then(detect_pi_version))
        .flatten();
    BinaryProbe {
        executable,
        pi_version,
    }
}

#[cfg(test)]
fn reset_binary_probe_test_state() {
    let harness = binary_probe_test_harness();
    harness.threads.lock().unwrap().clear();
    *harness.r#override.lock().unwrap() = None;
    harness.hits.store(0, Ordering::SeqCst);
    let mut cache = probe_cache().lock().unwrap();
    cache.values.clear();
    cache.inflight.clear();
}

#[cfg(test)]
pub(crate) fn with_binary_probe_test_state<R>(f: impl FnOnce() -> R) -> R {
    let harness = binary_probe_test_harness();
    let _guard = harness.lock.lock().unwrap();
    reset_binary_probe_test_state();
    let result = f();
    reset_binary_probe_test_state();
    result
}

#[cfg(test)]
pub(crate) fn binary_probe_test_count() -> usize {
    binary_probe_test_harness().hits.load(Ordering::SeqCst)
}

#[cfg(test)]
fn set_binary_probe_test_override(value: BinaryProbe) {
    let harness = binary_probe_test_harness();
    *harness.r#override.lock().unwrap() = Some(value);
}

#[cfg(test)]
pub(crate) fn participate_in_binary_probe_test() {
    binary_probe_test_harness()
        .threads
        .lock()
        .unwrap()
        .insert(std::thread::current().id());
}

/// Task readiness requires the active AskHuman transport to exist and be current. Prompt text and
/// Subagent Guard drift stay visible in integration settings but do not block `/new`.
fn integration_unavailable(target: AgentTarget, mode: agent_mode::Mode) -> bool {
    integration_unavailable_from(
        mode,
        agent_rules::is_installed(target),
        agent_mode::timeout_hook_supported(target),
        agent_mode::timeout_hook_is_installed(target),
        agent_mode::timeout_hook_needs_update(target),
        mcp_config::is_installed(target),
        mcp_config::needs_update(target),
    )
}

fn integration_unavailable_from(
    mode: agent_mode::Mode,
    rule_installed: bool,
    timeout_supported: bool,
    timeout_installed: bool,
    timeout_outdated: bool,
    mcp_installed: bool,
    mcp_outdated: bool,
) -> bool {
    match mode {
        agent_mode::Mode::None => true,
        agent_mode::Mode::Cli => {
            !rule_installed || (timeout_supported && (!timeout_installed || timeout_outdated))
        }
        agent_mode::Mode::Mcp => !rule_installed || !mcp_installed || mcp_outdated,
    }
}

pub fn all_readiness() -> Vec<AgentReadiness> {
    collect_readiness(None, false)
}

pub fn all_readiness_fresh() -> Vec<AgentReadiness> {
    collect_readiness(None, true)
}

pub fn collect_readiness(kind: Option<AgentKind>, force: bool) -> Vec<AgentReadiness> {
    let kinds: Vec<AgentKind> = kind
        .map(|kind| vec![kind])
        .unwrap_or_else(|| AgentKind::ALL.to_vec());
    std::thread::scope(|scope| {
        let handles: Vec<_> = kinds
            .into_iter()
            .map(|kind| scope.spawn(move || readiness_with(kind, force)))
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    })
}

/// Native fork capability is probed against the exact executable that will be stored in the
/// launch record. Cursor Agent CLI has no supported fork surface in V1.
pub fn fork_readiness(kind: AgentKind) -> ForkReadiness {
    const CACHE_TTL: Duration = Duration::from_secs(60);
    static CACHE: OnceLock<
        Mutex<std::collections::HashMap<AgentKind, (std::time::Instant, ForkReadiness)>>,
    > = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(std::collections::HashMap::new()));
    if let Some((at, value)) = cache.lock().unwrap().get(&kind) {
        if at.elapsed() < CACHE_TTL {
            return value.clone();
        }
    }
    let value = fork_readiness_uncached(kind);
    cache
        .lock()
        .unwrap()
        .insert(kind, (std::time::Instant::now(), value.clone()));
    value
}

fn fork_readiness_uncached(kind: AgentKind) -> ForkReadiness {
    if kind == AgentKind::Cursor {
        return ForkReadiness {
            kind,
            ready: false,
            diagnostics: vec!["Cursor Agent CLI does not support native session fork".into()],
        };
    }
    let base = readiness(kind);
    if !base.ready {
        return ForkReadiness {
            kind,
            ready: false,
            diagnostics: base.diagnostics,
        };
    }
    let Some(executable) = base.executable else {
        return ForkReadiness {
            kind,
            ready: false,
            diagnostics: vec!["Agent executable is unavailable".into()],
        };
    };
    let probe = match kind {
        AgentKind::Claude | AgentKind::Grok => {
            probe_help(&executable, &["--help"]).is_some_and(|text| text.contains("--fork-session"))
        }
        AgentKind::Codex => probe_help(&executable, &["fork", "--help"]).is_some_and(|text| {
            text.contains("SESSION_ID") && text.to_ascii_lowercase().contains("fork")
        }),
        AgentKind::Cursor => false,
        AgentKind::Pi => {
            probe_help(&executable, &["--help"]).is_some_and(|text| text.contains("--fork"))
        }
    };
    ForkReadiness {
        kind,
        ready: probe,
        diagnostics: (!probe)
            .then(|| format!("{} CLI does not expose native session fork", kind.label()))
            .into_iter()
            .collect(),
    }
}

pub fn all_fork_readiness() -> Vec<ForkReadiness> {
    std::thread::scope(|scope| {
        let handles: Vec<_> = AgentKind::ALL
            .into_iter()
            .map(|kind| scope.spawn(move || fork_readiness(kind)))
            .collect();
        handles
            .into_iter()
            .filter_map(|handle| handle.join().ok())
            .collect()
    })
}

#[cfg(target_os = "macos")]
pub fn terminal_available() -> bool {
    [
        "/System/Applications/Utilities/Terminal.app",
        "/Applications/Utilities/Terminal.app",
    ]
    .into_iter()
    .any(|path| Path::new(path).exists())
}

#[cfg(target_os = "windows")]
pub fn terminal_available() -> bool {
    resolve_windows_terminal().is_some()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn terminal_available() -> bool {
    false
}

/// Open a harmless platform terminal self-check without resolving or starting an Agent binary.
#[cfg(target_os = "macos")]
pub fn test_terminal() -> Result<()> {
    let script = r#"tell application "Terminal"
activate
do script "printf '\\nAskHuman Terminal test succeeded.\\n'"
end tell"#;
    let status = Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .status()
        .context("failed to ask Terminal.app to open a test window")?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| anyhow!("Terminal.app rejected the test"))
}

/// Open a harmless Windows Terminal tab using only fixed arguments.
#[cfg(target_os = "windows")]
pub fn test_terminal() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    let terminal = resolve_windows_terminal()
        .ok_or_else(|| anyhow!("Windows Terminal (wt.exe) is unavailable"))?;
    let status = Command::new(terminal)
        .args([
            "-w",
            "new",
            "new-tab",
            "--title",
            "AskHuman Test",
            "cmd.exe",
            "/d",
            "/k",
            "echo AskHuman Terminal test succeeded.",
        ])
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("failed to ask Windows Terminal to open a test window")?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| anyhow!("Windows Terminal rejected the test"))
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn test_terminal() -> Result<()> {
    Err(anyhow!(
        "Agent task terminal launch is unsupported on this platform"
    ))
}

pub fn cleanup_expired_records() {
    let dir = paths::agent_launch_dir();
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let now = epoch_secs();
    for entry in entries.flatten() {
        let path = entry.path();
        let keep = path.extension().and_then(|value| value.to_str()) == Some("json")
            && fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<LaunchRecord>(&bytes).ok())
                .is_some_and(|record| record.expires_at >= now);
        if !keep {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn create_record(
    source: LaunchSource,
    cwd: &Path,
    kind: AgentKind,
    permission: LaunchPermission,
    task: &str,
) -> Result<LaunchRecord> {
    create_record_with_files(source, cwd, kind, permission, task, &[], &[])
}

pub fn create_record_with_files(
    source: LaunchSource,
    cwd: &Path,
    kind: AgentKind,
    permission: LaunchPermission,
    task: &str,
    files: &[String],
    warnings: &[String],
) -> Result<LaunchRecord> {
    create_record_internal(
        source,
        cwd,
        kind,
        permission,
        task,
        files,
        warnings,
        LaunchMode::New,
    )
}

pub fn create_fork_record(
    source: LaunchSource,
    cwd: &Path,
    kind: AgentKind,
    permission: LaunchPermission,
    source_session_id: &str,
    task: &str,
) -> Result<LaunchRecord> {
    validate_source_session_id(source_session_id)?;
    if crate::agents::transcript_full::transcript_mtime(kind, source_session_id).is_none() {
        return Err(anyhow!("source session transcript is unavailable"));
    }
    create_record_internal(
        source,
        cwd,
        kind,
        permission,
        task,
        &[],
        &[],
        LaunchMode::Fork {
            source_session_id: source_session_id.to_string(),
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn create_record_internal(
    source: LaunchSource,
    cwd: &Path,
    kind: AgentKind,
    permission: LaunchPermission,
    task: &str,
    files: &[String],
    warnings: &[String],
    launch_mode: LaunchMode,
) -> Result<LaunchRecord> {
    if kind == AgentKind::Pi && permission != LaunchPermission::AgentDefault {
        return Err(anyhow!(
            "Pi does not expose a built-in permission mode; use AgentDefault"
        ));
    }
    let task = task.trim();
    if task.is_empty() {
        return Err(anyhow!("task must not be empty"));
    }
    if task.chars().count() > MAX_TASK_CHARS {
        return Err(anyhow!("task exceeds {MAX_TASK_CHARS} characters"));
    }
    if files.len() > crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO {
        return Err(anyhow!("too many task attachments"));
    }
    if files
        .iter()
        .chain(warnings.iter())
        .any(|value| value.contains(['\0', '\r']))
    {
        return Err(anyhow!(
            "attachment payload contains unsupported control characters"
        ));
    }
    let cwd = fs::canonicalize(cwd).context("failed to resolve workspace")?;
    if !cwd.is_dir() {
        return Err(anyhow!("workspace is not a directory"));
    }
    let status = readiness(kind);
    if !status.ready {
        return Err(anyhow!(status.diagnostics.join("; ")));
    }
    let executable = status
        .executable
        .ok_or_else(|| anyhow!("Agent executable unavailable"))?;
    if matches!(launch_mode, LaunchMode::Fork { .. }) {
        let fork = fork_readiness(kind);
        if !fork.ready {
            return Err(anyhow!(fork.diagnostics.join("; ")));
        }
    }
    let askhuman_executable = std::env::current_exe()
        .context("failed to resolve AskHuman executable")?
        .to_string_lossy()
        .to_string();
    let created_at = epoch_secs();
    let record = LaunchRecord {
        id: uuid::Uuid::new_v4().to_string(),
        created_at,
        expires_at: created_at + RECORD_TTL_SECS,
        source,
        task: task.to_string(),
        task_sha256: sha256(task.as_bytes()),
        files: files.to_vec(),
        warnings: warnings.to_vec(),
        payload_sha256: payload_sha256(task, files, warnings, &launch_mode),
        cwd: cwd.to_string_lossy().to_string(),
        kind,
        permission,
        executable,
        askhuman_executable,
        launch_mode,
    };
    write_private_record(&record)?;
    Ok(record)
}

/// Open a new Terminal.app window for an existing launch record. This never starts an Agent in the
/// current process; the one-time helper in the new terminal claims the record first.
#[cfg(target_os = "macos")]
pub fn open_terminal(record: &LaunchRecord) -> Result<()> {
    let command = terminal_helper_command(&record.askhuman_executable, &record.id);
    // `do script <command>` can inject before a newly created login shell has finished enabling
    // job control. A long-running TUI may then be treated as a background job and receive SIGTTOU
    // on its first terminal write. Create the tab first and wait for its startup command to become
    // idle before sending the one-time helper command.
    let script = r#"on run argv
tell application "Terminal"
  set launchTab to do script ""
  repeat while busy of launchTab
    delay 0.05
  end repeat
  delay 0.1
  do script (item 1 of argv) in launchTab
end tell
end run"#;
    let status = Command::new("/usr/bin/osascript")
        .args(["-e", script, &command])
        .status()
        .context("failed to ask Terminal.app to open a window")?;
    if !status.success() {
        return Err(anyhow!("Terminal.app rejected the launch request"));
    }
    Ok(())
}

/// Open a uniquely named Windows Terminal window and pass only the trusted AskHuman executable
/// plus the opaque launch token. A fixed, application-suppressed tab title gives the focus adapter
/// an exact target without exposing the task or relying on a mutable tab index.
#[cfg(target_os = "windows")]
pub fn open_terminal(record: &LaunchRecord) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    let terminal = resolve_windows_terminal()
        .ok_or_else(|| anyhow!("Windows Terminal (wt.exe) is required to launch Agent tasks"))?;
    let (window_name, tab_title) =
        super::terminal_focus::windows_terminal_identity(&record.id).map_err(anyhow::Error::msg)?;
    let mut command = Command::new(terminal);
    command
        .args(["-w", &window_name, "new-tab", "--title", &tab_title])
        .arg("--suppressApplicationTitle")
        .arg("--startingDirectory")
        .arg(&record.cwd)
        .arg(&record.askhuman_executable)
        .arg("__agent-launch")
        .arg(&record.id)
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let status = command
        .status()
        .context("failed to ask Windows Terminal to open a window")?;
    if !status.success() {
        return Err(anyhow!("Windows Terminal rejected the launch request"));
    }
    Ok(())
}

fn terminal_helper_command(askhuman_executable: &str, launch_id: &str) -> String {
    // Terminal.app can accept the second `do script` during the short handoff from the login
    // shell to its line editor. In that race it may discard the first injected character. Keep
    // sacrificial shell whitespace ahead of the quoted executable so either outcome is valid.
    format!(
        "  {} __agent-launch {}",
        shell_quote(askhuman_executable),
        shell_quote(launch_id)
    )
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub fn open_terminal(_record: &LaunchRecord) -> Result<()> {
    Err(anyhow!("IM Agent launch is unsupported on this platform"))
}

/// Hidden helper entry point. Returns only on validation failure; success replaces this process
/// with the selected Agent so it inherits the terminal's real TTY.
pub fn run_helper(args: &[String]) -> Result<()> {
    let token = args
        .first()
        .ok_or_else(|| anyhow!("missing launch token"))?;
    let record = claim_record(token)?;
    validate_claim(&record, token)?;
    #[cfg(windows)]
    {
        let action = if super::terminal_focus::register_windows_terminal_window(&record.id).is_ok()
        {
            "window_registered"
        } else {
            "window_registration_failed"
        };
        crate::daemon::lifecycle::log_runtime_event("windows_terminal", action, None);
    }
    std::env::set_current_dir(&record.cwd).context("failed to enter workspace")?;
    let mut command = Command::new(&record.executable);
    command.env(LAUNCH_ID_ENV, &record.id);
    command.args(agent_args(
        record.kind,
        record.permission,
        &record.launch_mode,
        &task_with_attachments(&record.task, &record.files, &record.warnings),
    ));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        Err(command.exec()).context("failed to start Agent")
    }
    #[cfg(windows)]
    {
        let status = command.status().context("failed to start Agent")?;
        if status.success() {
            Ok(())
        } else {
            Err(anyhow!("Agent exited with {status}"))
        }
    }
    #[cfg(not(any(unix, windows)))]
    Err(anyhow!("Agent launch is unsupported on this platform"))
}

fn write_private_record(record: &LaunchRecord) -> Result<()> {
    let dir = paths::agent_launch_dir();
    fs::create_dir_all(&dir)?;
    harden(&dir, 0o700);
    let path = record_path(&record.id);
    let tmp = dir.join(format!(".{}.tmp", record.id));
    fs::write(&tmp, serde_json::to_vec(record)?)?;
    harden(&tmp, 0o600);
    fs::rename(tmp, path)?;
    Ok(())
}

fn claim_record(token: &str) -> Result<LaunchRecord> {
    validate_token(token)?;
    let source = record_path(token);
    let claimed = paths::agent_launch_dir().join(format!("{token}.claimed"));
    fs::rename(&source, &claimed).context("launch record is missing or already claimed")?;
    let bytes = fs::read(&claimed)?;
    let _ = fs::remove_file(&claimed);
    serde_json::from_slice(&bytes).context("invalid launch record")
}

fn validate_claim(record: &LaunchRecord, token: &str) -> Result<()> {
    if record.id != token || epoch_secs() > record.expires_at {
        return Err(anyhow!("launch record expired or mismatched"));
    }
    if sha256(record.task.as_bytes()) != record.task_sha256 {
        return Err(anyhow!("launch record task hash mismatch"));
    }
    if !record.payload_sha256.is_empty() {
        let expected = payload_sha256(
            &record.task,
            &record.files,
            &record.warnings,
            &record.launch_mode,
        );
        let legacy = legacy_payload_sha256(&record.task, &record.files, &record.warnings);
        if record.payload_sha256 != expected
            && !(record.launch_mode == LaunchMode::New && record.payload_sha256 == legacy)
        {
            return Err(anyhow!("launch record payload hash mismatch"));
        }
    }
    if let LaunchMode::Fork { source_session_id } = &record.launch_mode {
        validate_source_session_id(source_session_id)?;
        if crate::agents::transcript_full::transcript_mtime(record.kind, source_session_id)
            .is_none()
        {
            return Err(anyhow!("source session transcript is unavailable"));
        }
        let fork = fork_readiness(record.kind);
        if !fork.ready {
            return Err(anyhow!(fork.diagnostics.join("; ")));
        }
    }
    let cwd = fs::canonicalize(&record.cwd).context("workspace is no longer available")?;
    if !crate::path_identity::equivalent(&cwd.to_string_lossy(), &record.cwd) {
        return Err(anyhow!("workspace path changed after launch was requested"));
    }
    let executable =
        fs::canonicalize(&record.executable).context("Agent executable is unavailable")?;
    if !crate::path_identity::equivalent(&executable.to_string_lossy(), &record.executable)
        || !is_executable(&executable)
    {
        return Err(anyhow!(
            "Agent executable changed after launch was requested"
        ));
    }
    let current = std::env::current_exe()?;
    if !crate::path_identity::equivalent(&current.to_string_lossy(), &record.askhuman_executable) {
        return Err(anyhow!(
            "AskHuman executable changed after launch was requested"
        ));
    }
    Ok(())
}

fn payload_sha256(
    task: &str,
    files: &[String],
    warnings: &[String],
    launch_mode: &LaunchMode,
) -> String {
    let payload = serde_json::to_vec(&(task, files, warnings, launch_mode)).unwrap_or_default();
    sha256(&payload)
}

fn legacy_payload_sha256(task: &str, files: &[String], warnings: &[String]) -> String {
    let payload = serde_json::to_vec(&(task, files, warnings)).unwrap_or_default();
    sha256(&payload)
}

fn validate_source_session_id(session_id: &str) -> Result<()> {
    if session_id.is_empty()
        || session_id.len() > 256
        || session_id
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(anyhow!("invalid source session id"));
    }
    Ok(())
}

fn agent_args(
    kind: AgentKind,
    permission: LaunchPermission,
    mode: &LaunchMode,
    prompt: &str,
) -> Vec<String> {
    let mut args = Vec::new();
    match mode {
        LaunchMode::New => {
            if permission == LaunchPermission::Yolo {
                args.push(yolo_flag(kind).into());
            }
            args.push(prompt.into());
        }
        LaunchMode::Fork { source_session_id } => match kind {
            AgentKind::Claude | AgentKind::Grok => {
                if permission == LaunchPermission::Yolo {
                    args.push(yolo_flag(kind).into());
                }
                args.extend([
                    "--resume".into(),
                    source_session_id.clone(),
                    "--fork-session".into(),
                    "--".into(),
                    prompt.into(),
                ]);
            }
            AgentKind::Codex => {
                args.push("fork".into());
                if permission == LaunchPermission::Yolo {
                    args.push(yolo_flag(kind).into());
                }
                args.extend(["--".into(), source_session_id.clone(), prompt.into()]);
            }
            AgentKind::Cursor => {
                // Creation rejects this mode through `fork_readiness`; keep helper fail-closed.
            }
            AgentKind::Pi => {
                args.extend(["--fork".into(), source_session_id.clone(), prompt.into()]);
            }
        },
    }
    args
}

fn probe_help(executable: &str, args: &[&str]) -> Option<String> {
    // GUI hosts often have a sparse PATH. Homebrew/npm Agent entrypoints may be scripts with an
    // `#!/usr/bin/env node` shebang, so probing the canonical script directly can fail even though
    // the login shell can launch it. Only the previously resolved executable and fixed help args
    // enter this shell; source session ids and user prompts never use this path.
    #[cfg(unix)]
    let mut child = {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|value| Path::new(value).is_absolute())
            .unwrap_or_else(|| "/bin/zsh".to_string());
        let command = std::iter::once(executable)
            .chain(args.iter().copied())
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ");
        Command::new(shell)
            .args(["-lc", &command])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .ok()?
    };
    #[cfg(windows)]
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    #[cfg(not(any(unix, windows)))]
    return None;
    let started = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let output = child.wait_with_output().ok()?;
                if !status.success() {
                    return None;
                }
                let mut bytes = output.stdout;
                bytes.extend(output.stderr);
                return String::from_utf8(bytes).ok();
            }
            Ok(None) if started.elapsed() < RESOLVE_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub fn task_with_attachments(task: &str, files: &[String], warnings: &[String]) -> String {
    let mut output = task.to_string();
    let available: Vec<&String> = files
        .iter()
        .filter(|path| Path::new(path.as_str()).is_file())
        .collect();
    let mut runtime_warnings = warnings.to_vec();
    for path in files {
        if !Path::new(path).is_file() {
            runtime_warnings.push(format!("Attachment became unavailable: {path}"));
        }
    }
    if !available.is_empty() {
        output.push_str("\n\nAttachments (local file paths):\n");
        for path in available {
            let quoted = serde_json::to_string(path).unwrap_or_else(|_| format!("\"{path}\""));
            output.push_str("- ");
            output.push_str(&quoted);
            output.push('\n');
        }
        output.push_str("Open and use these files as inputs for this task.");
    }
    if let Some(block) = crate::todo_attachments::warning_block(&runtime_warnings) {
        output.push_str("\n\n");
        output.push_str(&block);
    }
    output
}

fn resolve_login_shell_executable(name: &str) -> Option<String> {
    #[cfg(windows)]
    {
        resolve_windows_executable(name)
    }
    #[cfg(unix)]
    {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|v| Path::new(v).is_absolute())
            .unwrap_or_else(|| "/bin/zsh".to_string());
        let mut child = Command::new(shell)
            .args([
                "-lic",
                &format!("p=$(command -v {name}) && printf '\\n__ASKHUMAN_BIN__%s\\n' \"$p\""),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if !status.success() {
                        return None;
                    }
                    let output = child.wait_with_output().ok()?;
                    return output
                        .stdout
                        .split(|b| *b == b'\n')
                        .filter_map(|line| std::str::from_utf8(line).ok())
                        .map(str::trim)
                        .filter_map(|line| line.strip_prefix("__ASKHUMAN_BIN__"))
                        .filter(|line| Path::new(line).is_absolute())
                        .map(PathBuf::from)
                        .find_map(|path| fs::canonicalize(path).ok())
                        .filter(|path| is_executable(path))
                        .map(|path| path.to_string_lossy().to_string());
                }
                Ok(None) if started.elapsed() < RESOLVE_TIMEOUT => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    None
}

#[cfg(windows)]
pub(crate) fn resolve_windows_terminal() -> Option<String> {
    // Microsoft Store execution aliases are zero-byte reparse points. Rust canonicalization can
    // reject them even though CreateProcess resolves them correctly, so prefer the fixed per-user
    // WindowsApps alias instead of treating it like a regular Agent executable.
    let alias = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)?
        .join("Microsoft")
        .join("WindowsApps")
        .join("wt.exe");
    if fs::symlink_metadata(&alias).is_ok() {
        return Some(alias.to_string_lossy().to_string());
    }
    resolve_windows_executable("wt.exe")
}

#[cfg(windows)]
fn resolve_windows_executable(name: &str) -> Option<String> {
    let output = Command::new("where.exe")
        .arg(name)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        // npm installs both an extensionless POSIX shim and a `.cmd` launcher. `where.exe`
        // returns the POSIX shim first, but CreateProcess rejects it with ERROR_BAD_EXE_FORMAT.
        .filter(|path| is_windows_launchable(path))
        .find_map(|path| fs::canonicalize(path).ok())
        .filter(|path| is_windows_launchable(path))
        .map(|path| path.to_string_lossy().to_string())
}

fn target(kind: AgentKind) -> AgentTarget {
    match kind {
        AgentKind::Claude => AgentTarget::ClaudeCode,
        AgentKind::Codex => AgentTarget::Codex,
        AgentKind::Cursor => AgentTarget::Cursor,
        AgentKind::Grok => AgentTarget::Grok,
        AgentKind::Pi => AgentTarget::Pi,
    }
}

fn command_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Cursor => "cursor-agent",
        AgentKind::Grok => "grok",
        AgentKind::Pi => "pi",
    }
}

fn yolo_flag(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "--dangerously-skip-permissions",
        AgentKind::Codex => "--dangerously-bypass-approvals-and-sandbox",
        AgentKind::Cursor => "--yolo",
        AgentKind::Grok => "--always-approve",
        AgentKind::Pi => "",
    }
}

fn detect_pi_version(executable: &str) -> Option<(u64, u64, u64)> {
    let output = probe_help(executable, &["--version"])?;
    parse_pi_version(&output)
}

fn parse_pi_version(output: &str) -> Option<(u64, u64, u64)> {
    output
        .split(|ch: char| !(ch.is_ascii_digit() || ch == '.'))
        .filter(|part| !part.is_empty())
        .find_map(|part| {
            let mut numbers = part.split('.').map(str::parse::<u64>);
            let major = numbers.next()?.ok()?;
            let minor = numbers.next()?.ok()?;
            let patch = numbers.next().transpose().ok()?.unwrap_or(0);
            Some((major, minor, patch))
        })
}

fn pi_version_supported(version: (u64, u64, u64)) -> bool {
    version >= PI_MINIMUM
}

fn record_path(token: &str) -> PathBuf {
    paths::agent_launch_dir().join(format!("{token}.json"))
}

fn validate_token(token: &str) -> Result<()> {
    uuid::Uuid::parse_str(token).context("invalid launch token")?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.is_file() && fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

fn is_windows_launchable(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| {
                matches!(
                    extension.to_ascii_lowercase().as_str(),
                    "exe" | "com" | "cmd" | "bat"
                )
            })
}

#[cfg(windows)]
fn is_executable(path: &Path) -> bool {
    is_windows_launchable(path)
}

#[cfg(not(any(unix, windows)))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

#[cfg(unix)]
fn harden(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn harden(_path: &Path, _mode: u32) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yolo_flags_are_fixed() {
        assert_eq!(
            yolo_flag(AgentKind::Claude),
            "--dangerously-skip-permissions"
        );
        assert_eq!(
            yolo_flag(AgentKind::Codex),
            "--dangerously-bypass-approvals-and-sandbox"
        );
        assert_eq!(yolo_flag(AgentKind::Cursor), "--yolo");
        assert_eq!(yolo_flag(AgentKind::Grok), "--always-approve");
        assert_eq!(yolo_flag(AgentKind::Pi), "");
    }

    #[test]
    fn fork_arguments_are_fixed_and_keep_prompt_as_one_argv() {
        let prompt = "-$(touch /tmp/never)\nsecond line";
        let fork = LaunchMode::Fork {
            source_session_id: "source-123".into(),
        };
        assert_eq!(
            agent_args(
                AgentKind::Claude,
                LaunchPermission::AgentDefault,
                &fork,
                prompt
            ),
            vec!["--resume", "source-123", "--fork-session", "--", prompt]
        );
        assert_eq!(
            agent_args(AgentKind::Codex, LaunchPermission::Yolo, &fork, prompt),
            vec![
                "fork",
                "--dangerously-bypass-approvals-and-sandbox",
                "--",
                "source-123",
                prompt,
            ]
        );
        assert_eq!(
            agent_args(AgentKind::Grok, LaunchPermission::Yolo, &fork, prompt),
            vec![
                "--always-approve",
                "--resume",
                "source-123",
                "--fork-session",
                "--",
                prompt,
            ]
        );
        assert!(agent_args(
            AgentKind::Cursor,
            LaunchPermission::AgentDefault,
            &fork,
            prompt
        )
        .is_empty());
        assert_eq!(
            agent_args(AgentKind::Pi, LaunchPermission::AgentDefault, &fork, prompt),
            vec!["--fork", "source-123", prompt]
        );
    }

    #[test]
    fn pi_version_gate_accepts_082_and_newer() {
        assert_eq!(parse_pi_version("pi 0.82.0"), Some((0, 82, 0)));
        assert_eq!(parse_pi_version("v1.3.4\n"), Some((1, 3, 4)));
        assert_eq!(parse_pi_version("pi version 0.81"), Some((0, 81, 0)));
        assert!(pi_version_supported((0, 82, 0)));
        assert!(pi_version_supported((0, 83, 0)));
        assert!(!pi_version_supported((0, 81, 99)));
        assert_eq!(parse_pi_version("unknown"), None);
    }

    #[test]
    fn binary_probe_cache_reuses_until_force() {
        with_binary_probe_test_state(|| {
            participate_in_binary_probe_test();
            set_binary_probe_test_override(BinaryProbe {
                executable: Some("/tmp/pi-test".into()),
                pi_version: Some((0, 82, 0)),
            });
            let first = binary_probe(AgentKind::Pi, false);
            let second = binary_probe(AgentKind::Pi, false);
            assert_eq!(first, second);
            assert_eq!(binary_probe_test_count(), 1);
            let _forced = binary_probe(AgentKind::Pi, true);
            assert_eq!(binary_probe_test_count(), 2);
        });
    }

    #[test]
    fn binary_probe_single_flight_shares_one_uncached_call() {
        with_binary_probe_test_state(|| {
            set_binary_probe_test_override(BinaryProbe {
                executable: Some("/tmp/grok-test".into()),
                pi_version: None,
            });
            std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    participate_in_binary_probe_test();
                    binary_probe(AgentKind::Grok, false)
                });
                let second = scope.spawn(|| {
                    participate_in_binary_probe_test();
                    binary_probe(AgentKind::Grok, false)
                });
                first.join().unwrap();
                second.join().unwrap();
            });
            assert_eq!(binary_probe_test_count(), 1);
        });
    }

    #[test]
    fn legacy_launch_record_defaults_to_new_mode() {
        let value = serde_json::json!({
            "id": "4f37c6d8-7397-458c-8203-65a165395dae",
            "createdAt": 1,
            "expiresAt": 2,
            "source": { "channel": "popup", "target": "" },
            "task": "continue",
            "taskSha256": "hash",
            "cwd": "/tmp",
            "kind": "claude",
            "permission": "agent-default",
            "executable": "/usr/bin/claude",
            "askhumanExecutable": "/usr/bin/AskHuman"
        });
        let record: LaunchRecord = serde_json::from_value(value).unwrap();
        assert_eq!(record.launch_mode, LaunchMode::New);
    }

    #[test]
    fn shell_quote_handles_apostrophes() {
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
    }

    #[test]
    fn terminal_helper_command_has_sacrificial_whitespace() {
        assert_eq!(
            terminal_helper_command("/tmp/Ask Human", "launch-id"),
            "  '/tmp/Ask Human' __agent-launch 'launch-id'"
        );
    }

    #[test]
    fn windows_launchable_paths_reject_posix_npm_shims() {
        let temp = tempfile::tempdir().unwrap();
        let posix_shim = temp.path().join("codex");
        let command_shim = temp.path().join("codex.CMD");
        let executable = temp.path().join("codex.exe");
        let powershell = temp.path().join("codex.ps1");
        for path in [&posix_shim, &command_shim, &executable, &powershell] {
            fs::write(path, b"placeholder").unwrap();
        }
        assert!(!is_windows_launchable(&posix_shim));
        assert!(is_windows_launchable(&command_shim));
        assert!(is_windows_launchable(&executable));
        assert!(!is_windows_launchable(&powershell));
    }

    /// Task boundary validation happens before any filesystem/readiness side effect
    /// (spec gui-agent-task-launch §2.5 shares this path with IM /new).
    #[test]
    fn create_record_validates_task_boundaries_first() {
        let source = || LaunchSource {
            channel: "test".to_string(),
            target: String::new(),
        };
        let cwd = Path::new("/nonexistent-askhuman-test-dir");
        let err = create_record(
            source(),
            cwd,
            AgentKind::Claude,
            LaunchPermission::AgentDefault,
            "   ",
        )
        .unwrap_err();
        assert!(err.to_string().contains("must not be empty"));

        let over = "a".repeat(MAX_TASK_CHARS + 1);
        let err = create_record(
            source(),
            cwd,
            AgentKind::Claude,
            LaunchPermission::AgentDefault,
            &over,
        )
        .unwrap_err();
        assert!(err.to_string().contains("exceeds"));

        // Exactly at the limit passes length validation: with a nonexistent cwd the
        // next check (workspace resolution) fails instead, with no record written.
        let exact = "a".repeat(MAX_TASK_CHARS);
        let err = create_record(
            source(),
            cwd,
            AgentKind::Claude,
            LaunchPermission::AgentDefault,
            &exact,
        )
        .unwrap_err();
        assert!(err.to_string().contains("workspace"));
    }

    #[test]
    fn pi_rejects_permission_override_before_launch_side_effects() {
        let error = create_record(
            LaunchSource {
                channel: "test".into(),
                target: String::new(),
            },
            Path::new("/nonexistent-askhuman-test-dir"),
            AgentKind::Pi,
            LaunchPermission::Yolo,
            "task",
        )
        .unwrap_err();
        assert!(error.to_string().contains("built-in permission mode"));
    }

    #[test]
    fn task_hash_is_stable() {
        assert_eq!(
            sha256(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn attachment_prompt_quotes_paths_and_reports_files_that_disappear() {
        let temp = tempfile::tempdir().unwrap();
        let existing = temp.path().join("a file.md");
        fs::write(&existing, b"x").unwrap();
        let missing = temp.path().join("gone.md");
        let prompt = task_with_attachments(
            "review",
            &[
                existing.to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ],
            &["Earlier warning".into()],
        );
        assert!(prompt.contains("Attachments (local file paths):"));
        assert!(prompt.contains(&serde_json::to_string(&existing).unwrap()));
        assert!(prompt.contains("Earlier warning"));
        assert!(prompt.contains("Attachment became unavailable"));
    }

    #[test]
    fn readiness_ignores_prompt_and_guard_freshness_but_requires_transport() {
        assert!(!integration_unavailable_from(
            agent_mode::Mode::Cli,
            true,
            true,
            true,
            false,
            false,
            false,
        ));
        assert!(!integration_unavailable_from(
            agent_mode::Mode::Mcp,
            true,
            false,
            false,
            false,
            true,
            false,
        ));
        assert!(integration_unavailable_from(
            agent_mode::Mode::Cli,
            true,
            true,
            false,
            false,
            false,
            false,
        ));
        assert!(integration_unavailable_from(
            agent_mode::Mode::Mcp,
            true,
            false,
            false,
            false,
            true,
            true,
        ));
    }

    /// Local installation contract probe. Ignored in CI because the Codex binary/integration is
    /// optional; run explicitly when diagnosing a reported `forkReady=false`.
    #[test]
    #[ignore]
    fn real_codex_fork_help_when_available() {
        let executable = resolve_login_shell_executable("codex").expect("Codex is not installed");
        let help =
            probe_help(&executable, &["fork", "--help"]).expect("Codex fork help probe failed");
        assert!(help.contains("SESSION_ID"), "{help}");
    }
}
