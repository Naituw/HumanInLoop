//! Secure Terminal.app launch bridge for tasks created from IM.
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
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const RECORD_TTL_SECS: u64 = 5 * 60;
const MAX_TASK_CHARS: usize = 3000;
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(2);
pub const LAUNCH_ID_ENV: &str = "ASKHUMAN_AGENT_TASK_LAUNCH_ID";

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
    pub binary_ready: bool,
    pub lifecycle_ready: bool,
    pub integration_ready: bool,
    pub integration_mode: String,
    pub ready: bool,
    pub diagnostics: Vec<String>,
}

pub fn readiness(kind: AgentKind) -> AgentReadiness {
    let command = command_name(kind).to_string();
    let executable = resolve_login_shell_executable(&command);
    let lifecycle = agent_lifecycle::status(kind);
    let target = target(kind);
    let mode = agent_mode::current(target);
    let integration_ready = !integration_unavailable(target, mode);
    let binary_ready = executable.is_some();
    let lifecycle_ready = lifecycle.supported && lifecycle.installed && !lifecycle.outdated;
    let mut diagnostics = Vec::new();
    if !binary_ready {
        diagnostics.push(format!(
            "{} CLI was not found in the login shell",
            kind.label()
        ));
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
        binary_ready,
        lifecycle_ready,
        integration_ready,
        integration_mode: mode.as_str().to_string(),
        ready: binary_ready && lifecycle_ready && integration_ready,
        diagnostics,
    }
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
    std::thread::scope(|scope| {
        let handles: Vec<_> = AgentKind::ALL
            .into_iter()
            .map(|kind| scope.spawn(move || readiness(kind)))
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

pub fn terminal_available() -> bool {
    cfg!(target_os = "macos")
        && [
            "/System/Applications/Utilities/Terminal.app",
            "/Applications/Utilities/Terminal.app",
        ]
        .into_iter()
        .any(|path| Path::new(path).exists())
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
    let command = format!(
        "{} __agent-launch {}",
        shell_quote(&record.askhuman_executable),
        shell_quote(&record.id)
    );
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

#[cfg(not(target_os = "macos"))]
pub fn open_terminal(_record: &LaunchRecord) -> Result<()> {
    Err(anyhow!(
        "IM Agent launch currently requires macOS Terminal.app"
    ))
}

/// Hidden helper entry point. Returns only on validation failure; success replaces this process
/// with the selected Agent so it inherits the terminal's real TTY.
#[cfg(unix)]
pub fn run_helper(args: &[String]) -> Result<()> {
    let token = args
        .first()
        .ok_or_else(|| anyhow!("missing launch token"))?;
    let record = claim_record(token)?;
    validate_claim(&record, token)?;
    std::env::set_current_dir(&record.cwd).context("failed to enter workspace")?;
    let mut command = Command::new(&record.executable);
    command.env(LAUNCH_ID_ENV, &record.id);
    command.args(agent_args(
        record.kind,
        record.permission,
        &record.launch_mode,
        &task_with_attachments(&record.task, &record.files, &record.warnings),
    ));
    use std::os::unix::process::CommandExt;
    Err(command.exec()).context("failed to start Agent")
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
    if cwd.to_string_lossy() != record.cwd {
        return Err(anyhow!("workspace path changed after launch was requested"));
    }
    let executable =
        fs::canonicalize(&record.executable).context("Agent executable is unavailable")?;
    if executable.to_string_lossy() != record.executable || !is_executable(&executable) {
        return Err(anyhow!(
            "Agent executable changed after launch was requested"
        ));
    }
    let current = std::env::current_exe()?;
    if current.to_string_lossy() != record.askhuman_executable {
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
        },
    }
    args
}

fn probe_help(executable: &str, args: &[&str]) -> Option<String> {
    // GUI hosts often have a sparse PATH. Homebrew/npm Agent entrypoints may be scripts with an
    // `#!/usr/bin/env node` shebang, so probing the canonical script directly can fail even though
    // the login shell can launch it. Only the previously resolved executable and fixed help args
    // enter this shell; source session ids and user prompts never use this path.
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|value| Path::new(value).is_absolute())
        .unwrap_or_else(|| "/bin/zsh".to_string());
    let command = std::iter::once(executable)
        .chain(args.iter().copied())
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    let mut child = Command::new(shell)
        // Login startup restores GUI hosts' sparse PATH; remaining non-interactive avoids job
        // control and arbitrary interactive prompt behavior inside the Terminal launch helper.
        .args(["-lc", &command])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
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

fn target(kind: AgentKind) -> AgentTarget {
    match kind {
        AgentKind::Claude => AgentTarget::ClaudeCode,
        AgentKind::Codex => AgentTarget::Codex,
        AgentKind::Cursor => AgentTarget::Cursor,
        AgentKind::Grok => AgentTarget::Grok,
    }
}

fn command_name(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "claude",
        AgentKind::Codex => "codex",
        AgentKind::Cursor => "cursor-agent",
        AgentKind::Grok => "grok",
    }
}

fn yolo_flag(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "--dangerously-skip-permissions",
        AgentKind::Codex => "--dangerously-bypass-approvals-and-sandbox",
        AgentKind::Cursor => "--yolo",
        AgentKind::Grok => "--always-approve",
    }
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

#[cfg(not(unix))]
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
