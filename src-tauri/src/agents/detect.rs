//! 运行时识别真实 Agent、从 env 取会话 ID、向上 walk 进程树定位 Agent 进程、kill-0 探活。
//!
//! 复刻 `demo/agent-lifecycle/harness/common.cjs` 的实测逻辑（FINDINGS §7.6 / §1.1）：
//! - `detect_running_agent`：从 hook/ask 子进程 env 判定真实 Agent（解决 Cursor 双触发去重）。
//!   顺序 **必须** 先判 Cursor（它也会设 `CLAUDE_PROJECT_DIR`），再 Codex，再 Claude。
//! - `walk_agent_pid`：从本进程向上回溯进程树，取第一个命中 Agent token、且非自身的祖先 pid。
//! - `pid_alive`：`kill(pid, 0)`（unix）判存活。

use std::collections::HashMap;

use super::AgentKind;

/// 进程链节点：pid / 父 pid / 可执行名(comm) / 完整命令行(command)。
#[derive(Debug, Clone)]
struct ProcEntry {
    pid: u32,
    ppid: u32,
    comm: String,
    command: String,
}

/// Native process facts used by lifecycle binding. Fields that the OS refuses to disclose stay
/// `None`; callers must fail closed rather than substituting another process's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub parent_pid: u32,
    pub executable: Option<String>,
    pub command_line: Option<String>,
    pub session_id: Option<u32>,
    /// Windows FILETIME ticks since 1601; unavailable on Unix adapters today.
    pub creation_time: Option<u64>,
}

/// 本程序自身可执行文件名的标记（避免把 reporter / daemon / mcp / ask 子进程误判成 Agent）。
/// **只按可执行名匹配**（comm + argv0 basename），**不扫描参数**——否则命令行参数里恰好提到
/// "askhuman" 的 agent（如 `codex exec "用 askhuman 提问…"`）会被误判为自身而被 walk 跳过，
/// 导致 walk 上溯命中外层 agent 或漏报。我们自己的进程 argv0 恒为 `AskHuman`（含旧名 humaninloop），
/// 据此即可精准自识别；reporter 的 `__agent-hook` 是参数、其 argv0 同样是 AskHuman，故无需单列。
const SELF_MARKERS: [&str; 2] = ["askhuman", "humaninloop"];

/// 从一个 env map 判定真实运行的 Agent（去重判据，见 FINDINGS §7.6）。
///
/// `CLAUDE_PROJECT_DIR` **不可** 作判据——Cursor 兼容性也会设它，故必须先判 Cursor。
/// 判不出返回 `None`（调用方应按 intended 处理，避免漏报）。
pub fn detect_running_agent_from(env: &HashMap<String, String>) -> Option<AgentKind> {
    let has = |k: &str| env.contains_key(k);
    if env
        .get("PI_CODING_AGENT")
        .is_some_and(|value| value == "true")
        || has("PI_SESSION_ID")
        || has("PI_SESSION_FILE")
    {
        return Some(AgentKind::Pi);
    }
    // Grok **必须**最先判：它在**每个** hook 子进程都注入 `CLAUDE_PROJECT_DIR`（Claude 兼容别名），
    // 并会合并触发 `~/.claude`/`~/.cursor` 的兼容 hook。凭 `GROK_HOOK_EVENT`（hook runner 恒注入）/
    // `GROK_SESSION_ID` 认出真实家族是 Grok，配合 reporter 的「running==Grok 且 intended!=Grok 跳过」
    // 去重，避免 Grok 会话被错标成 Claude/Cursor（FINDINGS：兼容读取的坑）。
    if has("GROK_HOOK_EVENT") || has("GROK_SESSION_ID") || has("GROK_WORKSPACE_ROOT") {
        return Some(AgentKind::Grok);
    }
    if has("CURSOR_AGENT") || has("CURSOR_VERSION") || has("CURSOR_PROJECT_DIR") {
        return Some(AgentKind::Cursor);
    }
    if env.keys().any(|k| k.starts_with("CODEX_")) {
        return Some(AgentKind::Codex);
    }
    if has("CLAUDECODE") || has("CLAUDE_CODE_SESSION_ID") {
        return Some(AgentKind::Claude);
    }
    None
}

/// 读取本进程 env 判定真实 Agent。
pub fn detect_running_agent() -> Option<AgentKind> {
    detect_running_agent_from(&current_env())
}

/// Identify the Agent that directly invoked a short-lived CLI command. Environment markers are
/// fast and precise; the process-tree fallback covers MCP launchers that clear Agent variables.
/// Unlike the latency-sensitive ask path, metadata-only commands can afford this synchronous walk.
pub fn detect_invoking_agent() -> Option<AgentKind> {
    detect_running_agent().or_else(|| walk_any_agent_from_self().map(|(kind, _)| kind))
}

/// 各家会话 ID 的 env 变量名（shell 工具子进程注入；hook 子进程通常无，靠 stdin）。
pub fn session_id_env_var(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Claude => "CLAUDE_CODE_SESSION_ID",
        AgentKind::Codex => "CODEX_THREAD_ID",
        AgentKind::Cursor => "CURSOR_CONVERSATION_ID",
        // Grok 在每个 hook 子进程注入 `GROK_SESSION_ID`（见 grok hooks 文档）。
        AgentKind::Grok => "GROK_SESSION_ID",
        AgentKind::Pi => "PI_SESSION_ID",
    }
}

/// 从一个 env map 取指定家族的会话 ID。
pub fn session_id_from_env_map(kind: AgentKind, env: &HashMap<String, String>) -> Option<String> {
    env.get(session_id_env_var(kind))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// 读取本进程 env 取会话 ID。
pub fn session_id_from_env(kind: AgentKind) -> Option<String> {
    session_id_from_env_map(kind, &current_env())
}

fn current_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

/// 识别一个进程节点是否「属于」指定家族的 Agent 进程。
///
/// - Claude / Codex：可执行名(comm) 子串含 `claude` / `codex`，或 argv0 basename 等于之。
/// - Cursor：cursor-agent 的可执行名是 `agent`（argv0 basename == `agent`），或命令行特异含 `cursor-agent`。
fn matches_agent(entry: &ProcEntry, kind: AgentKind) -> bool {
    let comm = entry.comm.to_ascii_lowercase();
    let command = entry.command.to_ascii_lowercase();
    let argv0_base = command
        .split_whitespace()
        .next()
        .map(basename)
        .unwrap_or_default();
    match kind {
        AgentKind::Claude => comm.contains("claude") || argv0_base == "claude",
        AgentKind::Codex => comm.contains("codex") || argv0_base == "codex",
        AgentKind::Cursor => {
            argv0_base == "agent"
                || comm.contains("cursor-agent")
                || command.contains("cursor-agent")
        }
        // Grok 可执行名为 `grok`（软链）或 `grok-macos-*`（真身），故按子串 `grok` 匹配。
        AgentKind::Grok => comm.contains("grok") || argv0_base.contains("grok"),
        // Keep Pi matching exact: the two-letter name appears in ordinary arguments frequently.
        AgentKind::Pi => {
            argv0_base == "pi"
                || argv0_base == "pi.js"
                || command.contains("/pi-coding-agent/")
                || command.contains("\\pi-coding-agent\\")
        }
    }
}

/// 识别一个进程节点是否为 Codex「共享 app-server 守护」而非 TUI 本体（spec D25/D27）。
///
/// 新版 Codex TUI 经 Unix domain socket 连一个**长寿共享 app-server 守护**跑 agent，hook / 工具 /
/// MCP 子进程都跑在 app-server 的进程树里 → 从子进程向上 walk **只会命中 app-server、永远拿不到
/// TUI pid**；且该守护多会话共用、可 reparent 到 PID 1。故把它的 pid 当作「无可用会话 pid」
/// （`walk_agent_pid` 命中它即返回 None，让该会话落到 registry 的「无 pid」路径 = 同 Claude 被
/// PID-scrub 时）。
///
/// 判据（D27 主判据）：命令行里基名为 `codex` 的令牌之后，**跳过前导全局选项**，第一个非选项
/// token 是 `app-server` 子命令（覆盖 `codex app-server …`、`node <path>/codex app-server …`，
/// 以及 ChatGPT / Codex Desktop 的 `codex -c features.…=true app-server …`）。
///
/// 只认「codex 后的子命令位」而非「参数里任意出现 app-server」，以免把提示词里恰好含
/// "app-server" 的 TUI（如 `codex exec "用 app-server 提问"`）误判。嵌入 / 旧模式 TUI 命令为纯
/// `codex`（子命令是 `exec`/`resume`/无）→ 返回 false，pid 照常可用。
fn is_shared_app_server(entry: &ProcEntry) -> bool {
    let command = entry.command.to_ascii_lowercase();
    let tokens: Vec<&str> = command.split_whitespace().collect();
    tokens.iter().enumerate().any(|(i, tok)| {
        basename(tok) == "codex" && codex_subcommand(&tokens[i + 1..]) == Some("app-server")
    })
}

/// Codex CLI 在 argv0（或包装路径中的 `…/codex`）之后、**子命令之前**常见的取值型全局选项。
/// 命中时需连同下一 token（选项值）一起跳过，才能正确定位子命令（D27 / ChatGPT Desktop）。
const CODEX_VALUE_OPTS: &[&str] = &[
    "-c",
    "--config",
    "-m",
    "--model",
    "-p",
    "--profile",
    "--cdn-base-url",
];

/// 从 `codex` 之后的参数里取出**第一个非选项 token**（即子命令位）；全是选项则 `None`。
///
/// 跳过规则：`--flag=value` / `-c=value` 计一个 token；已知取值型选项（见 `CODEX_VALUE_OPTS`）
/// 再吞掉紧随的值；其余以 `-` 开头的当作 boolean / 未知 flag 只跳自身。这样
/// `codex -c features.code_mode_host=true app-server …` 的子命令仍是 `app-server`，而
/// `codex exec … app-server …` 的子命令仍是 `exec`。
fn codex_subcommand<'a>(args: &[&'a str]) -> Option<&'a str> {
    let mut i = 0;
    while i < args.len() {
        let t = args[i];
        if !t.starts_with('-') {
            return Some(t);
        }
        if t.contains('=') {
            i += 1;
            continue;
        }
        if CODEX_VALUE_OPTS.contains(&t) {
            i += 2;
            continue;
        }
        i += 1;
    }
    None
}

fn is_self(entry: &ProcEntry) -> bool {
    let comm = entry.comm.to_ascii_lowercase();
    let argv0_base = entry
        .command
        .split_whitespace()
        .next()
        .map(basename)
        .unwrap_or_default()
        .to_ascii_lowercase();
    SELF_MARKERS
        .iter()
        .any(|m| comm.contains(m) || argv0_base.contains(m))
}

fn basename(p: &str) -> String {
    let name = p
        .trim_matches('"')
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(p)
        .trim_matches('"');
    name.strip_suffix(".exe").unwrap_or(name).to_string()
}

/// 从 `start_pid` 向上回溯进程树，返回第一个命中指定家族、且非自身的祖先 pid。
/// 找不到时返回 `None` → 调用方落 TTL 兜底。
///
/// spec D25/D27：Codex 命中的祖先若是**共享 app-server 守护**（`is_shared_app_server`）→ 返回
/// `None`（walk 只会命中它、拿不到 TUI pid；该会话按「无 pid」路径治理，见 registry）。
pub fn walk_agent_pid(kind: AgentKind, start_pid: u32) -> Option<u32> {
    let chain = process_chain(start_pid);
    let matched = chain
        .into_iter()
        .filter(|e| !is_self(e))
        .find(|e| matches_agent(e, kind))?;
    if kind == AgentKind::Codex && is_shared_app_server(&matched) {
        return None;
    }
    Some(matched.pid)
}

/// 从当前进程向上 walk 定位指定家族的 Agent pid。
pub fn walk_agent_pid_from_self(kind: AgentKind) -> Option<u32> {
    walk_agent_pid(kind, std::process::id())
}

/// 从 `start_pid` 向上回溯，返回最近的（非自身）Agent 祖先及其家族。
///
/// 用于 **env 判不出家族** 的兜底——典型为 MCP 模式：agent 启动 STDIO MCP server 时
/// `env_clear()`，子进程看不到任何 `CODEX_*`/`CURSOR_*`/`CLAUDE*` 变量（既判不出家族、也拿不到
/// 会话 ID），但进程树依旧能定位到 agent 本体。返回的 pid 是当次现取、真实存活的（可用作 registry
/// 按 pid 匹配的键）；拿不到 `session_id`。
pub fn walk_any_agent(start_pid: u32) -> Option<(AgentKind, u32)> {
    const KINDS: [AgentKind; 5] = [
        AgentKind::Pi,
        AgentKind::Grok,
        AgentKind::Codex,
        AgentKind::Claude,
        AgentKind::Cursor,
    ];
    process_chain(start_pid)
        .into_iter()
        .filter(|e| !is_self(e))
        .find_map(|e| {
            let kind = KINDS.iter().copied().find(|&k| matches_agent(&e, k))?;
            // spec D25/D27：跳过 Codex 共享 app-server（返回 None 让 find_map 继续上溯；其上通常即
            // PID 1，最终返回 None → 不按共享 pid 做 by-pid 刷新，规避跨 session 串味）。
            if kind == AgentKind::Codex && is_shared_app_server(&e) {
                return None;
            }
            Some((kind, e.pid))
        })
}

/// 从当前进程向上 walk 定位最近的任意家族 Agent 祖先（kind + pid）。
pub fn walk_any_agent_from_self() -> Option<(AgentKind, u32)> {
    walk_any_agent(std::process::id())
}

/// Return the direct parent process id using the platform process snapshot.
pub fn parent_pid(pid: u32) -> Option<u32> {
    process_chain(pid)
        .first()
        .map(|entry| entry.ppid)
        .filter(|parent| *parent != 0)
}

/// Inspect one process without launching PowerShell, WMI, or another helper process on Windows.
pub fn inspect_process(pid: u32) -> Option<ProcessIdentity> {
    #[cfg(unix)]
    {
        let (parent_pid, executable) = ps_ppid_comm(pid)?;
        Some(ProcessIdentity {
            pid,
            parent_pid,
            executable: (!executable.is_empty()).then_some(executable),
            command_line: ps_command(pid),
            session_id: None,
            creation_time: None,
        })
    }
    #[cfg(windows)]
    {
        inspect_process_windows(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        None
    }
}

/// 识别 `pid` 所在的终端 App：沿进程链向上找首个已知终端祖先，返回稳定标识串
/// （`apple-terminal` / `iterm2` / `ghostty` / `kitty` / `wezterm` / `alacritty` / `tmux`
/// / `vscode` / `cursor`）；找不到返回 `None`。
///
/// 供 Agent 状态窗口「聚焦终端」按钮**按支持度显隐**：前端仅对已支持的终端（v1 = `apple-terminal`）
/// 展示按钮。tmux 在外层终端之前命中（pane 与外层 Tab 不是同一个，单纯聚焦外层 Tab 不准），故视为
/// 暂不支持。
pub fn terminal_kind(pid: u32) -> Option<&'static str> {
    process_chain(pid).iter().find_map(terminal_of_entry)
}

/// 单个进程节点是否属于某已知终端（按 comm + 完整命令行的小写子串匹配）。
fn terminal_of_entry(e: &ProcEntry) -> Option<&'static str> {
    let s = format!("{} {}", e.comm, e.command).to_ascii_lowercase();
    // 优先匹配具体 `.app/` 路径，再退宽松名；各分支互斥，命中即返回。
    if s.contains("terminal.app/") {
        return Some("apple-terminal");
    }
    if s.contains("iterm.app/") || s.contains("iterm2") {
        return Some("iterm2");
    }
    if s.contains("windowsterminal.exe") || s.contains("windows terminal") {
        return Some("windows-terminal");
    }
    if s.contains("ghostty") {
        return Some("ghostty");
    }
    if s.contains("wezterm") {
        return Some("wezterm");
    }
    if s.contains("alacritty") {
        return Some("alacritty");
    }
    if s.contains("kitty") {
        return Some("kitty");
    }
    if s.contains("tmux") {
        return Some("tmux");
    }
    if s.contains("cursor.app/") || s.contains("cursor helper") {
        return Some("cursor");
    }
    if s.contains("visual studio code") || s.contains("code.app/") || s.contains("code helper") {
        return Some("vscode");
    }
    None
}

/// 进程是否存活（`kill(pid, 0)`：Ok / EPERM 视为存活，ESRCH 为已死）。
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if r == 0 {
        return true;
    }
    // errno: EPERM(1) 存在但无权限 → 存活；ESRCH(3) → 已死。
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED as i32);
    }
    let mut exit_code = 0u32;
    let ok = unsafe { GetExitCodeProcess(process, &mut exit_code) } != 0;
    unsafe {
        CloseHandle(process);
    }
    ok && exit_code == STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
pub fn pid_alive(_pid: u32) -> bool {
    false
}

// ── Process ancestry (Unix: ps; Windows: Toolhelp + native process queries) ──

#[cfg(unix)]
fn process_chain(start_pid: u32) -> Vec<ProcEntry> {
    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pid = start_pid;
    while pid > 1 && seen.insert(pid) {
        let Some((ppid, comm)) = ps_ppid_comm(pid) else {
            break;
        };
        let command = ps_command(pid).unwrap_or_default();
        chain.push(ProcEntry {
            pid,
            ppid,
            comm,
            command,
        });
        if ppid == 0 {
            break;
        }
        pid = ppid;
    }
    chain
}

#[cfg(windows)]
fn process_chain(start_pid: u32) -> Vec<ProcEntry> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut table = HashMap::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        let end = entry
            .szExeFile
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(entry.szExeFile.len());
        let name = String::from_utf16_lossy(&entry.szExeFile[..end]);
        table.insert(entry.th32ProcessID, (entry.th32ParentProcessID, name));
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }

    let mut chain = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut pid = start_pid;
    while pid > 0 && seen.insert(pid) {
        let Some((ppid, name)) = table.get(&pid).cloned() else {
            break;
        };
        let native = inspect_process_windows_with_parent(pid, ppid);
        let comm = native
            .as_ref()
            .and_then(|process| process.executable.clone())
            .unwrap_or_else(|| name.clone());
        let command = native
            .and_then(|process| process.command_line)
            .unwrap_or(name);
        chain.push(ProcEntry {
            pid,
            ppid,
            comm,
            command,
        });
        pid = ppid;
    }
    chain
}

#[cfg(windows)]
fn inspect_process_windows(pid: u32) -> Option<ProcessIdentity> {
    inspect_process_windows_with_parent(pid, toolhelp_parent_pid(pid)?)
}

#[cfg(windows)]
fn inspect_process_windows_with_parent(pid: u32, parent_pid: u32) -> Option<ProcessIdentity> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return None;
    }
    let executable = query_process_image(process);
    let command_line = query_process_command_line(process);
    let mut session_id = 0u32;
    let session_id =
        (unsafe { ProcessIdToSessionId(pid, &mut session_id) } != 0).then_some(session_id);
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let creation_time =
        (unsafe { GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) }
            != 0)
            .then_some(((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64);
    unsafe {
        CloseHandle(process);
    }

    Some(ProcessIdentity {
        pid,
        parent_pid,
        executable,
        command_line,
        session_id,
        creation_time,
    })
}

#[cfg(windows)]
fn query_process_image(process: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    use windows_sys::Win32::System::Threading::QueryFullProcessImageNameW;

    let mut buffer = vec![0u16; 32_768];
    let mut length = buffer.len() as u32;
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) } == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length as usize]))
}

#[cfg(windows)]
fn query_process_command_line(process: windows_sys::Win32::Foundation::HANDLE) -> Option<String> {
    #[repr(C)]
    struct UnicodeString {
        length: u16,
        maximum_length: u16,
        buffer: *const u16,
    }

    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process_handle: windows_sys::Win32::Foundation::HANDLE,
            process_information_class: u32,
            process_information: *mut std::ffi::c_void,
            process_information_length: u32,
            return_length: *mut u32,
        ) -> i32;
    }

    const PROCESS_COMMAND_LINE_INFORMATION: u32 = 60;
    let mut required = 0u32;
    unsafe {
        NtQueryInformationProcess(
            process,
            PROCESS_COMMAND_LINE_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut required,
        );
    }
    if required < std::mem::size_of::<UnicodeString>() as u32 {
        return None;
    }
    let mut storage = vec![0u8; required as usize];
    let status = unsafe {
        NtQueryInformationProcess(
            process,
            PROCESS_COMMAND_LINE_INFORMATION,
            storage.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    };
    if status < 0 {
        return None;
    }
    let value = unsafe { &*(storage.as_ptr().cast::<UnicodeString>()) };
    let byte_len = value.length as usize;
    if byte_len == 0 || byte_len % 2 != 0 || value.buffer.is_null() {
        return None;
    }
    let storage_start = storage.as_ptr() as usize;
    let storage_end = storage_start.checked_add(storage.len())?;
    let text_start = value.buffer as usize;
    let text_end = text_start.checked_add(byte_len)?;
    if text_start < storage_start || text_end > storage_end {
        return None;
    }
    Some(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(value.buffer, byte_len / 2)
    }))
}

#[cfg(windows)]
fn toolhelp_parent_pid(pid: u32) -> Option<u32> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found = None;
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        if entry.th32ProcessID == pid {
            found = Some(entry.th32ParentProcessID);
            break;
        }
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe {
        CloseHandle(snapshot);
    }
    found
}

#[cfg(not(any(unix, windows)))]
fn process_chain(_start_pid: u32) -> Vec<ProcEntry> {
    Vec::new()
}

/// `ps -o ppid=,comm= -p <pid>` → (ppid, comm)。
#[cfg(unix)]
fn ps_ppid_comm(pid: u32) -> Option<(u32, String)> {
    let out = run_ps(&["-o", "ppid=,comm=", "-p", &pid.to_string()])?;
    let trimmed = out.trim();
    let mut it = trimmed.splitn(2, char::is_whitespace);
    let ppid = it.next()?.trim().parse::<u32>().ok()?;
    let comm = it.next().unwrap_or("").trim().to_string();
    Some((ppid, comm))
}

#[cfg(unix)]
fn ps_command(pid: u32) -> Option<String> {
    run_ps(&["-o", "command=", "-p", &pid.to_string()]).map(|s| s.trim().to_string())
}

#[cfg(unix)]
fn run_ps(args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("ps").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).to_string();
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn detect_cursor_takes_priority_over_claude_project_dir() {
        // Cursor 兼容性会设 CLAUDE_PROJECT_DIR，但必须判成 cursor。
        let env = env_of(&[("CURSOR_AGENT", "1"), ("CLAUDE_PROJECT_DIR", "/x")]);
        assert_eq!(detect_running_agent_from(&env), Some(AgentKind::Cursor));
    }

    #[test]
    fn detect_pi_from_native_session_environment() {
        let env = env_of(&[
            ("PI_CODING_AGENT", "1"),
            ("PI_SESSION_ID", "pi-session"),
            ("PI_SESSION_FILE", "/tmp/pi-session.jsonl"),
            ("CLAUDE_PROJECT_DIR", "/x"),
        ]);
        assert_eq!(detect_running_agent_from(&env), Some(AgentKind::Pi));
        assert_eq!(
            session_id_from_env_map(AgentKind::Pi, &env),
            Some("pi-session".to_string())
        );
    }

    #[test]
    fn detect_grok_takes_priority_over_claude_and_cursor_compat() {
        // Grok 在每个 hook 都设 CLAUDE_PROJECT_DIR；触发 claude/cursor 兼容 hook 时 env 还可能带
        // CURSOR_*，但只要有 GROK_HOOK_EVENT / GROK_SESSION_ID 就必须判成 Grok（供 reporter 去重）。
        let env = env_of(&[
            ("GROK_SESSION_ID", "gs1"),
            ("GROK_HOOK_EVENT", "session_start"),
            ("CLAUDE_PROJECT_DIR", "/x"),
            ("CURSOR_AGENT", "1"),
        ]);
        assert_eq!(detect_running_agent_from(&env), Some(AgentKind::Grok));
        assert_eq!(
            session_id_from_env_map(AgentKind::Grok, &env),
            Some("gs1".to_string())
        );
    }

    #[test]
    fn detect_codex_by_prefix() {
        let env = env_of(&[("CODEX_MANAGED_BY_NPM", "1")]);
        assert_eq!(detect_running_agent_from(&env), Some(AgentKind::Codex));
    }

    #[test]
    fn detect_claude_when_only_claude_markers() {
        let env = env_of(&[("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", "abc")]);
        assert_eq!(detect_running_agent_from(&env), Some(AgentKind::Claude));
    }

    #[test]
    fn detect_none_when_ambiguous() {
        // 仅 CLAUDE_PROJECT_DIR 不足以判定（Cursor 也设它）。
        let env = env_of(&[("CLAUDE_PROJECT_DIR", "/x")]);
        assert_eq!(detect_running_agent_from(&env), None);
    }

    #[test]
    fn session_id_from_env_reads_per_kind_var() {
        let env = env_of(&[("CODEX_THREAD_ID", " tid ")]);
        assert_eq!(
            session_id_from_env_map(AgentKind::Codex, &env),
            Some("tid".to_string())
        );
        assert_eq!(session_id_from_env_map(AgentKind::Claude, &env), None);
    }

    #[test]
    fn matches_agent_recognizes_cursor_agent_named_agent() {
        let e = ProcEntry {
            pid: 1,
            ppid: 0,
            comm: "/Users/u/.local/bin/agent".to_string(),
            command: "agent --use-system-ca /x/index.js --yolo".to_string(),
        };
        assert!(matches_agent(&e, AgentKind::Cursor));
        assert!(!matches_agent(&e, AgentKind::Claude));
    }

    #[test]
    fn terminal_detection_recognizes_windows_terminal_process() {
        let entry = ProcEntry {
            pid: 42,
            ppid: 1,
            comm: r"C:\Program Files\WindowsApps\Microsoft.WindowsTerminal\WindowsTerminal.exe"
                .to_string(),
            command: "Windows Terminal".to_string(),
        };
        assert_eq!(terminal_of_entry(&entry), Some("windows-terminal"));
    }

    #[test]
    fn self_marker_excluded() {
        let e = ProcEntry {
            pid: 1,
            ppid: 0,
            comm: "AskHuman".to_string(),
            command: "AskHuman __agent-hook cursor turn-start".to_string(),
        };
        assert!(is_self(&e));
        // 完整安装路径作为 comm（detect.rs 的 `ps -o comm=` 取完整路径）同样应识别为自身。
        let e2 = ProcEntry {
            pid: 2,
            ppid: 0,
            comm: "/Users/u/.local/bin/AskHuman".to_string(),
            command: "/Users/u/.local/bin/AskHuman mcp".to_string(),
        };
        assert!(is_self(&e2));
    }

    #[test]
    fn shared_app_server_detected_by_command_token() {
        // 共享 app-server 守护：子命令位为 app-server（unix / stdio 皆算）。
        let unix = ProcEntry {
            pid: 52407,
            ppid: 1,
            comm: "codex".to_string(),
            command: "/opt/homebrew/lib/.../bin/codex app-server --listen unix://".to_string(),
        };
        assert!(is_shared_app_server(&unix));
        let stdio = ProcEntry {
            pid: 39788,
            ppid: 39755,
            comm: "codex".to_string(),
            command: "/Applications/Codex.app/.../codex app-server --listen stdio://".to_string(),
        };
        assert!(is_shared_app_server(&stdio));
        // node 包装器：`node <path>/codex app-server …`——codex 后子命令 app-server 也算。
        let wrapper = ProcEntry {
            pid: 52404,
            ppid: 1,
            comm: "node".to_string(),
            command: "node /opt/homebrew/bin/codex app-server --listen unix://".to_string(),
        };
        assert!(is_shared_app_server(&wrapper));
        // ChatGPT / Codex Desktop：`codex -c features.…=true app-server …`——全局 -c 插在子命令前。
        let desktop = ProcEntry {
            pid: 98289,
            ppid: 98175,
            comm: "codex".to_string(),
            command: "/Applications/ChatGPT.app/Contents/Resources/codex -c features.code_mode_host=true app-server --analytics-default-enabled".to_string(),
        };
        assert!(is_shared_app_server(&desktop));
        // `--config=value` 合并写法、以及布尔 flag 夹在中间。
        let config_eq = ProcEntry {
            pid: 1,
            ppid: 1,
            comm: "codex".to_string(),
            command:
                "codex --config=features.code_mode_host=true --analytics-default-enabled app-server"
                    .to_string(),
        };
        assert!(is_shared_app_server(&config_eq));
    }

    #[test]
    fn plain_codex_tui_is_not_app_server() {
        // 嵌入 / 旧模式 TUI：纯 codex（无 app-server 子命令）→ 不算共享守护，pid 照常可用。
        for cmd in [
            "/opt/homebrew/lib/.../bin/codex",
            "codex",
            "codex resume",
            "codex exec 用 app-server 关键词提问", // "app-server" 在提示词里，子命令仍是 exec
            // 真实 TUI：全局 flag + 用户 prompt，子命令位不是 app-server
            "codex --dangerously-bypass-approvals-and-sandbox 帮我分析一下提交",
            // -c 的值碰巧含 app-server 字样也不应误判（子命令缺省 / 另有子命令）
            "codex -c foo.app-server=true resume",
        ] {
            let e = ProcEntry {
                pid: 1,
                ppid: 2,
                comm: "codex".to_string(),
                command: cmd.to_string(),
            };
            assert!(!is_shared_app_server(&e), "should not flag: {cmd}");
        }
    }

    #[test]
    fn codex_subcommand_skips_leading_options() {
        assert_eq!(codex_subcommand(&["app-server"]), Some("app-server"));
        assert_eq!(
            codex_subcommand(&["-c", "features.code_mode_host=true", "app-server"]),
            Some("app-server")
        );
        assert_eq!(
            codex_subcommand(&["--config=x=y", "app-server", "--listen", "unix://"]),
            Some("app-server")
        );
        assert_eq!(
            codex_subcommand(&["exec", "用", "app-server", "提问"]),
            Some("exec")
        );
        assert_eq!(codex_subcommand(&["-c", "x=y"]), None);
    }

    #[test]
    fn non_codex_with_app_server_token_is_not_flagged() {
        // argv0 / comm 不是 codex，即便命令行恰好含 app-server 令牌也不误判。
        let e = ProcEntry {
            pid: 1,
            ppid: 2,
            comm: "node".to_string(),
            command: "node my-app-server app-server".to_string(),
        };
        assert!(!is_shared_app_server(&e));
    }

    #[test]
    fn is_self_ignores_marker_in_agent_args() {
        // codex 命令行**参数**里提到 "askhuman"（如 `codex exec "用 askhuman 提问"`）不应被误判为自身——
        // 否则 walk 会跳过真正的 codex、上溯命中外层 agent。仅按可执行名（comm/argv0）匹配即可避免。
        let e = ProcEntry {
            pid: 1,
            ppid: 0,
            comm: "/opt/homebrew/lib/node_modules/@openai/codex/.../bin/codex".to_string(),
            command: "/opt/homebrew/lib/.../bin/codex exec 用 askhuman 工具向我提问".to_string(),
        };
        assert!(!is_self(&e));
        assert!(matches_agent(&e, AgentKind::Codex));
    }
}
