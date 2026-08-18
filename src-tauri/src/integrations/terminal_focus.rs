//! Focus the exact terminal surface registered for an Agent session.
//!
//! macOS sessions keep the existing TTY-based Terminal.app/iTerm2 adapters. AskHuman-created
//! Windows sessions use a UUID window name plus a suppressed application title. The launch helper
//! captures that exact top-level HWND while tab 0 is active. Focus validates the persisted HWND,
//! process identity, and Windows Terminal owner before asking the known-live named window to select
//! tab 0; this prevents `wt.exe` from creating a replacement window for a stale identity.

const WINDOWS_TERMINAL_KIND: &str = "windows-terminal";

pub fn windows_terminal_kind() -> &'static str {
    WINDOWS_TERMINAL_KIND
}

pub fn windows_terminal_identity(launch_id: &str) -> Result<(String, String), String> {
    let id = uuid::Uuid::parse_str(launch_id)
        .map_err(|_| "focus terminal: invalid registered launch identity".to_string())?
        .hyphenated()
        .to_string();
    Ok((format!("askhuman-{id}"), format!("AskHuman Agent [{id}]")))
}

/// Focus the registered terminal. macOS requires an Agent pid; Windows requires the inherited
/// launch UUID recorded by the daemon when the AskHuman-created Agent first reports lifecycle.
pub fn focus_agent_terminal(pid: Option<u32>, launch_id: Option<&str>) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let _ = launch_id;
        let pid = pid.ok_or_else(|| "focus terminal: agent pid is unavailable".to_string())?;
        let dev = tty_device(pid)?;
        match crate::agents::detect::terminal_kind(pid) {
            Some("apple-terminal") => run_focus_script(&apple_terminal_script(&dev)),
            Some("iterm2") => run_focus_script(&iterm2_script(&dev)),
            other => Err(format!(
                "focus terminal: unsupported terminal {}",
                other.unwrap_or("unknown")
            )),
        }
    }
    #[cfg(target_os = "windows")]
    {
        let _ = pid;
        let launch_id = launch_id
            .ok_or_else(|| "focus terminal: no registered Windows Terminal identity".to_string())?;
        focus_windows_terminal(launch_id)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (pid, launch_id);
        Err("focus terminal: unsupported on this platform".to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowCandidate {
    handle: isize,
    title: String,
    class_name: String,
    executable: Option<String>,
    process_id: u32,
}

fn is_windows_terminal(candidate: &WindowCandidate) -> bool {
    candidate
        .executable
        .as_deref()
        .and_then(|path| path.rsplit(['/', '\\']).next())
        .is_some_and(|name| name.eq_ignore_ascii_case("WindowsTerminal.exe"))
}

fn select_registered_windows_terminal(
    candidates: &[WindowCandidate],
    expected_title: &str,
) -> Result<isize, String> {
    let matches: Vec<_> = candidates
        .iter()
        .filter(|candidate| candidate.title == expected_title)
        .filter(|candidate| is_windows_terminal(candidate))
        .collect();
    match matches.as_slice() {
        [candidate] => Ok(candidate.handle),
        [] => Err("focus terminal: registered Windows Terminal window was not found".to_string()),
        _ => Err("focus terminal: registered Windows Terminal identity is ambiguous".to_string()),
    }
}

#[cfg(target_os = "windows")]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsTerminalRegistration {
    launch_id: String,
    handle: u64,
    process_id: u32,
}

#[cfg(target_os = "windows")]
fn registration_path(launch_id: &str) -> std::path::PathBuf {
    crate::paths::state_dir()
        .join("windows-terminal")
        .join(format!("{launch_id}.json"))
}

/// Capture the dedicated Windows Terminal HWND while the launch tab's fixed title is active.
/// The helper runs inside that tab before starting the Agent, so this is the reliable point to
/// bind Windows Terminal's internal named window to a cross-process Win32 identity.
#[cfg(target_os = "windows")]
pub fn register_windows_terminal_window(launch_id: &str) -> Result<(), String> {
    let id = uuid::Uuid::parse_str(launch_id)
        .map_err(|_| "focus terminal: invalid registered launch identity".to_string())?
        .hyphenated()
        .to_string();
    let (_, expected_title) = windows_terminal_identity(&id)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let candidate = loop {
        let candidates = enumerate_windows(&expected_title)?;
        match select_registered_windows_terminal(&candidates, &expected_title) {
            Ok(handle) => {
                let candidate = candidates
                    .into_iter()
                    .find(|candidate| candidate.handle == handle)
                    .ok_or_else(|| "focus terminal: registered window disappeared".to_string())?;
                break candidate;
            }
            Err(error) if error.ends_with("was not found") => {}
            Err(error) => return Err(error),
        }
        if std::time::Instant::now() >= deadline {
            return Err("focus terminal: launched Windows Terminal window was not found".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };

    let registration = WindowsTerminalRegistration {
        launch_id: id.clone(),
        handle: candidate.handle as u64,
        process_id: candidate.process_id,
    };
    let path = registration_path(&id);
    let parent = path
        .parent()
        .ok_or_else(|| "focus terminal: invalid registration path".to_string())?;
    std::fs::create_dir_all(parent).map_err(|error| {
        format!("focus terminal: cannot create registration directory: {error}")
    })?;
    let temp = parent.join(format!(".{id}.tmp"));
    let bytes = serde_json::to_vec(&registration)
        .map_err(|error| format!("focus terminal: cannot encode registration: {error}"))?;
    std::fs::write(&temp, bytes)
        .map_err(|error| format!("focus terminal: cannot write registration: {error}"))?;
    std::fs::rename(&temp, &path)
        .map_err(|error| format!("focus terminal: cannot commit registration: {error}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn focus_windows_terminal(launch_id: &str) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::System::Threading::{
        AttachThreadInput, GetCurrentThreadId, CREATE_NO_WINDOW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsIconic, IsWindow,
        SetForegroundWindow, ShowWindowAsync, SW_RESTORE,
    };

    let (window_name, expected_title) = windows_terminal_identity(launch_id)?;
    let path = registration_path(launch_id);
    let registration: WindowsTerminalRegistration = std::fs::read(&path)
        .map_err(|_| "focus terminal: registered Windows Terminal window was not found".to_string())
        .and_then(|bytes| {
            serde_json::from_slice(&bytes)
                .map_err(|_| "focus terminal: invalid Windows Terminal registration".to_string())
        })?;
    if registration.launch_id != launch_id {
        return Err("focus terminal: Windows Terminal registration mismatch".into());
    }
    let hwnd = registration.handle as HWND;
    if unsafe { IsWindow(hwnd) } == 0 {
        let _ = std::fs::remove_file(&path);
        return Err("focus terminal: registered Windows Terminal window was not found".into());
    }
    let candidate = window_candidate(hwnd).ok_or_else(|| {
        let _ = std::fs::remove_file(&path);
        "focus terminal: registered Windows Terminal window was not found".to_string()
    })?;
    if candidate.process_id != registration.process_id || !is_windows_terminal(&candidate) {
        let _ = std::fs::remove_file(&path);
        return Err("focus terminal: stale Windows Terminal registration".into());
    }

    // The stored HWND proves the dedicated named window still exists, so routing this fixed
    // `focus-tab` command cannot create the phantom window that an unvalidated `wt -w` can.
    let terminal = super::agent_launch::resolve_windows_terminal()
        .ok_or_else(|| "focus terminal: wt.exe is unavailable".to_string())?;
    let status = std::process::Command::new(terminal)
        .args(["-w", &window_name, "focus-tab", "-t", "0"])
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .map_err(|error| format!("focus terminal: wt.exe failed: {error}"))?;
    if !status.success() {
        return Err("focus terminal: Windows Terminal rejected focus-tab".into());
    }

    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1500);
    while window_title(hwnd).as_deref() != Some(expected_title.as_str()) {
        if std::time::Instant::now() >= deadline {
            return Err("focus terminal: registered tab did not become active".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }

    unsafe {
        if IsIconic(hwnd) != 0 {
            ShowWindowAsync(hwnd, SW_RESTORE);
        }
        BringWindowToTop(hwnd);
        if SetForegroundWindow(hwnd) != 0 {
            return Ok(());
        }

        // Windows can reject a background Tauri worker despite a real user click. Temporarily
        // join the foreground and target input queues, retry activation, then always detach.
        let current_thread = GetCurrentThreadId();
        let foreground = GetForegroundWindow();
        let foreground_thread = if foreground.is_null() {
            0
        } else {
            GetWindowThreadProcessId(foreground, std::ptr::null_mut())
        };
        let target_thread = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());
        let attached_foreground = foreground_thread != 0
            && foreground_thread != current_thread
            && AttachThreadInput(current_thread, foreground_thread, 1) != 0;
        let attached_target = target_thread != 0
            && target_thread != current_thread
            && target_thread != foreground_thread
            && AttachThreadInput(current_thread, target_thread, 1) != 0;
        BringWindowToTop(hwnd);
        let focused = SetForegroundWindow(hwnd) != 0;
        if attached_target {
            AttachThreadInput(current_thread, target_thread, 0);
        }
        if attached_foreground {
            AttachThreadInput(current_thread, foreground_thread, 0);
        }
        if !focused {
            return Err("focus terminal: Windows refused to activate the registered window".into());
        }
    }
    Ok(())
}

#[cfg(target_os = "windows")]
struct WindowEnumeration<'a> {
    expected_title: &'a str,
    candidates: Vec<WindowCandidate>,
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn enum_window(
    hwnd: windows_sys::Win32::Foundation::HWND,
    lparam: windows_sys::Win32::Foundation::LPARAM,
) -> windows_sys::core::BOOL {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetClassNameW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    };

    let enumeration = &mut *(lparam as *mut WindowEnumeration<'_>);
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }
    let mut title = [0u16; 256];
    let title_len = GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32);
    if title_len <= 0 {
        return 1;
    }
    let title = String::from_utf16_lossy(&title[..title_len as usize]);
    if title != enumeration.expected_title {
        return 1;
    }
    let mut class_name = [0u16; 128];
    let class_len = GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32);
    let class_name = if class_len > 0 {
        String::from_utf16_lossy(&class_name[..class_len as usize])
    } else {
        String::new()
    };
    let mut pid = 0u32;
    GetWindowThreadProcessId(hwnd, &mut pid);
    let executable = crate::agents::detect::inspect_process(pid).and_then(|value| value.executable);
    enumeration.candidates.push(WindowCandidate {
        handle: hwnd as isize,
        title,
        class_name,
        executable,
        process_id: pid,
    });
    1
}

#[cfg(target_os = "windows")]
fn window_title(hwnd: windows_sys::Win32::Foundation::HWND) -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::GetWindowTextW;

    let mut title = [0u16; 256];
    let len = unsafe { GetWindowTextW(hwnd, title.as_mut_ptr(), title.len() as i32) };
    (len > 0).then(|| String::from_utf16_lossy(&title[..len as usize]))
}

#[cfg(target_os = "windows")]
fn window_candidate(hwnd: windows_sys::Win32::Foundation::HWND) -> Option<WindowCandidate> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetClassNameW, GetWindowThreadProcessId};

    let title = window_title(hwnd).unwrap_or_default();
    let mut class_name = [0u16; 128];
    let class_len =
        unsafe { GetClassNameW(hwnd, class_name.as_mut_ptr(), class_name.len() as i32) };
    let class_name = if class_len > 0 {
        String::from_utf16_lossy(&class_name[..class_len as usize])
    } else {
        String::new()
    };
    let mut process_id = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
    if process_id == 0 {
        return None;
    }
    let executable =
        crate::agents::detect::inspect_process(process_id).and_then(|value| value.executable);
    Some(WindowCandidate {
        handle: hwnd as isize,
        title,
        class_name,
        executable,
        process_id,
    })
}

#[cfg(target_os = "windows")]
fn enumerate_windows(expected_title: &str) -> Result<Vec<WindowCandidate>, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::EnumWindows;

    let mut enumeration = WindowEnumeration {
        expected_title,
        candidates: Vec::new(),
    };
    let ok = unsafe {
        EnumWindows(
            Some(enum_window),
            (&mut enumeration as *mut WindowEnumeration<'_>) as isize,
        )
    };
    if ok == 0 {
        return Err(format!(
            "focus terminal: failed to enumerate windows: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(enumeration.candidates)
}

/// Return the controlling terminal device, such as `/dev/ttys003`.
#[cfg(target_os = "macos")]
fn tty_device(pid: u32) -> Result<String, String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "tty=", "-p", &pid.to_string()])
        .output()
        .map_err(|e| format!("ps failed: {e}"))?;
    if !out.status.success() {
        return Err(format!("process {pid} not found"));
    }
    let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if tty.is_empty() || tty.contains('?') || tty == "-" {
        return Err(format!("process {pid} has no controlling terminal"));
    }
    Ok(if tty.starts_with("/dev/") {
        tty
    } else {
        format!("/dev/{tty}")
    })
}

#[cfg(target_os = "macos")]
fn apple_terminal_script(dev: &str) -> String {
    format!(
        r#"tell application "Terminal"
    set theTTY to "{dev}"
    set didFocus to false
    repeat with w in windows
        repeat with tb in tabs of w
            if (tty of tb) is theTTY then
                set selected of tb to true
                set frontmost of w to true
                set didFocus to true
            end if
        end repeat
    end repeat
    if didFocus then activate
    return didFocus
end tell"#
    )
}

#[cfg(target_os = "macos")]
fn iterm2_script(dev: &str) -> String {
    format!(
        r#"tell application id "com.googlecode.iterm2"
    set theTTY to "{dev}"
    set didFocus to false
    repeat with w in windows
        repeat with t in tabs of w
            repeat with s in sessions of t
                if (tty of s) is theTTY then
                    tell w to select
                    tell t to select
                    tell s to select
                    set didFocus to true
                end if
            end repeat
        end repeat
    end repeat
    if didFocus then activate
    return didFocus
end tell"#
    )
}

#[cfg(target_os = "macos")]
fn run_focus_script(script: &str) -> Result<(), String> {
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("osascript failed: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    if String::from_utf8_lossy(&out.stdout).trim() == "true" {
        Ok(())
    } else {
        Err("no matching terminal tab/session found".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "123e4567-e89b-12d3-a456-426614174000";

    #[test]
    fn windows_identity_is_stable_and_rejects_untrusted_values() {
        assert_eq!(
            windows_terminal_identity(ID).unwrap(),
            (
                "askhuman-123e4567-e89b-12d3-a456-426614174000".to_string(),
                "AskHuman Agent [123e4567-e89b-12d3-a456-426614174000]".to_string()
            )
        );
        assert!(windows_terminal_identity("not-a-uuid").is_err());
    }

    #[test]
    fn window_selection_requires_exact_title_and_terminal_executable() {
        let title = windows_terminal_identity(ID).unwrap().1;
        let values = vec![
            WindowCandidate {
                handle: 1,
                title: format!("prefix {title}"),
                class_name: "CASCADIA_HOSTING_WINDOW_CLASS".into(),
                executable: Some(r"C:\Program Files\WindowsApps\WindowsTerminal.exe".into()),
                process_id: 10,
            },
            WindowCandidate {
                handle: 2,
                title: title.clone(),
                class_name: "Other".into(),
                executable: Some(r"C:\Windows\notepad.exe".into()),
                process_id: 20,
            },
            WindowCandidate {
                handle: 3,
                title: title.clone(),
                class_name: "CASCADIA_HOSTING_WINDOW_CLASS".into(),
                executable: Some(r"C:\Program Files\WindowsApps\WindowsTerminal.exe".into()),
                process_id: 30,
            },
        ];
        assert_eq!(
            select_registered_windows_terminal(&values, &title).unwrap(),
            3
        );
    }

    #[test]
    fn window_selection_fails_closed_for_missing_or_ambiguous_identity() {
        let title = windows_terminal_identity(ID).unwrap().1;
        let candidate = WindowCandidate {
            handle: 1,
            title: title.clone(),
            class_name: "CASCADIA_HOSTING_WINDOW_CLASS".into(),
            executable: Some(r"C:\WindowsTerminal.exe".into()),
            process_id: 10,
        };
        assert!(select_registered_windows_terminal(&[], &title).is_err());
        assert!(
            select_registered_windows_terminal(&[candidate.clone(), candidate], &title).is_err()
        );
    }
}
