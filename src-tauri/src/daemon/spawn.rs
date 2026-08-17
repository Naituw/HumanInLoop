//! 后台拉起 Daemon：macOS 优先交给当前用户的 GUI launchd domain 管理，其它 Unix 则 detach
//! （新会话）并把 stdio 重定向到 daemon.log；Windows 使用无控制台的新进程组并写同一日志。
//!
//! macOS 不能在 Aqua 会话里直接 `setsid` 后长期运行：那样的 daemon 会跨用户登出残留，却仍持有
//! 已销毁 GUI 会话的 bootstrap namespace。用户重新登录后，它虽然还能通过 Unix socket 接收请求，
//! 但新拉起的 popup helper 无法向新 WindowServer / pasteboard 服务 check-in，最终表现为“进程存在但
//! 永远不弹窗”。因此只要 `gui/<uid>` 可用，就统一 bootstrap 到该 domain：既能静默读取登录钥匙串，
//! 也会在登出时随 GUI domain 一起退出。纯 headless 环境无法 bootstrap 时才回退 setsid。

pub fn spawn_detached() -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        if spawn_via_gui_launchd().is_ok() {
            return Ok(());
        }
        // GUI 域不可用（纯 headless）→ 回退原 setsid 拉起。
    }
    #[cfg(unix)]
    {
        spawn_plain_detached()
    }
    #[cfg(windows)]
    {
        spawn_windows_detached()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "daemon spawning is unsupported on this platform",
        ))
    }
}

#[cfg(windows)]
fn spawn_windows_detached() -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, PROCESS_INFORMATION,
        STARTUPINFOW,
    };

    let exe = std::env::current_exe()?;
    let mut application: Vec<u16> = exe.as_os_str().encode_wide().collect();
    application.push(0);

    // `std::process::Command` must enable handle inheritance when it wires redirected stdio.
    // On Windows that can retain an unrelated caller pipeline in the daemon and its GUI children,
    // so a script capturing `AskHuman daemon start` never observes EOF. Create the background role
    // with inheritance explicitly disabled; `--background` makes the child append logs itself.
    let mut command_line = Vec::new();
    command_line.push(b'"' as u16);
    command_line.extend(exe.as_os_str().encode_wide());
    command_line.push(b'"' as u16);
    command_line.extend(" daemon run --background".encode_utf16());
    command_line.push(0);

    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let mut process = PROCESS_INFORMATION::default();
    let created = unsafe {
        CreateProcessW(
            application.as_ptr(),
            command_line.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
            std::ptr::null(),
            std::ptr::null(),
            &startup,
            &mut process,
        )
    };
    if created == 0 {
        return Err(std::io::Error::last_os_error());
    }
    unsafe {
        CloseHandle(process.hThread);
        CloseHandle(process.hProcess);
    }
    Ok(())
}

/// Prevent background roles from opening a second console while keeping ordinary CLI output.
pub fn configure_background(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// 原始拉起方式：`setsid` 新建会话 + stdio 重定向到 daemon.log，直接继承当前会话上下文。
#[cfg(unix)]
fn spawn_plain_detached() -> std::io::Result<()> {
    use super::lifecycle;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()?;
    if let Some(dir) = lifecycle::log_path().parent() {
        std::fs::create_dir_all(dir)?;
    }
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(lifecycle::log_path())?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .arg("run")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));
    // 新建会话，彻底脱离调用方的控制终端 / 进程组（终端关闭不会带走 daemon）。
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn()?;
    Ok(())
}

// ===== macOS：经 GUI launchd 域拉起 =====

#[cfg(target_os = "macos")]
const DAEMON_LAUNCHD_LABEL: &str = "com.naituw.humaninloop.daemon";

/// Production 沿用登录项的固定 label；Dev Instance 按配置目录生成稳定后缀，避免多个工作树以及
/// production daemon 在同一个 `gui/<uid>` domain 内互相 bootout。
#[cfg(target_os = "macos")]
fn launchd_label(config_dir: &std::path::Path, isolated: bool) -> String {
    if !isolated {
        return DAEMON_LAUNCHD_LABEL.to_string();
    }

    use sha2::{Digest, Sha256};
    use std::fmt::Write as _;

    let digest = Sha256::digest(config_dir.to_string_lossy().as_bytes());
    let mut suffix = String::with_capacity(16);
    for byte in &digest[..8] {
        write!(&mut suffix, "{byte:02x}").expect("writing to String cannot fail");
    }
    format!("{DAEMON_LAUNCHD_LABEL}.instance.{suffix}")
}

/// 经 `gui/<uid>` launchd 域 bootstrap 一个跑 `daemon run` 的任务，使 daemon 落在 Aqua 会话。
///
/// 透传 HOME / TMPDIR / PATH 及全部 `ASKHUMAN_*` 环境变量，保住 perf/隔离调用方（隔离 HOME、
/// `ASKHUMAN_NO_KEYCHAIN`、mock API base 等）的语义。成功返回 `Ok(())`，否则 `Err`（调用方回退）。
#[cfg(target_os = "macos")]
fn spawn_via_gui_launchd() -> std::io::Result<()> {
    use super::lifecycle;
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()?;
    let log = lifecycle::log_path();
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let config_dir = crate::paths::config_dir();
    let label = launchd_label(&config_dir, crate::dev_instance::is_dev_instance());
    let plist_path = config_dir.join("daemon-launchd.plist");

    // 透传隔离/配置相关 env：HOME/TMPDIR/PATH + 全部 ASKHUMAN_*。
    let mut env_xml = String::new();
    for key in ["HOME", "TMPDIR", "PATH"] {
        if let Ok(v) = std::env::var(key) {
            env_xml.push_str(&plist_env_entry(key, &v));
        }
    }
    for (k, v) in std::env::vars() {
        if k.starts_with("ASKHUMAN_") {
            env_xml.push_str(&plist_env_entry(&k, &v));
        }
    }

    let plist = launchd_plist_contents(
        &exe.display().to_string(),
        &log.display().to_string(),
        &label,
        &env_xml,
    );
    std::fs::write(&plist_path, plist)?;

    // 自清理：先 bootout 上次残留的（已退出）任务，再 bootstrap 新的（RunAtLoad 立即启动）。
    let plist_str = plist_path.display().to_string();
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &domain, &plist_str])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let status = Command::new("/bin/launchctl")
        .args(["bootstrap", &domain, &plist_str])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(
            "launchctl bootstrap gui domain failed",
        ))
    }
}

#[cfg(target_os = "macos")]
fn launchd_plist_contents(exe: &str, log: &str, label: &str, env_xml: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>EnvironmentVariables</key>
    <dict>
{env_xml}    </dict>
</dict>
</plist>
"#,
        label = xml_escape(label),
        exe = xml_escape(exe),
        log = xml_escape(log),
    )
}

/// 生成一条 plist `EnvironmentVariables` 子项（key/value 均做 XML 转义）。
#[cfg(target_os = "macos")]
fn plist_env_entry(key: &str, value: &str) -> String {
    format!(
        "        <key>{}</key>\n        <string>{}</string>\n",
        xml_escape(key),
        xml_escape(value)
    )
}

/// 最小 XML 转义（路径/值理论上可能含 & < >）。
#[cfg(target_os = "macos")]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn production_and_isolated_launchd_labels_do_not_collide() {
        let production = launchd_label(Path::new("/Users/test/.askhuman"), false);
        let dev_a = launchd_label(Path::new("/work/a/.askhuman-dev/home"), true);
        let dev_a_again = launchd_label(Path::new("/work/a/.askhuman-dev/home"), true);
        let dev_b = launchd_label(Path::new("/work/b/.askhuman-dev/home"), true);

        assert_eq!(production, DAEMON_LAUNCHD_LABEL);
        assert_eq!(dev_a, dev_a_again);
        assert_ne!(dev_a, production);
        assert_ne!(dev_a, dev_b);
        assert!(dev_a.starts_with(&format!("{DAEMON_LAUNCHD_LABEL}.instance.")));
    }

    #[test]
    fn launchd_plist_binds_daemon_to_interactive_gui_job() {
        let plist = launchd_plist_contents(
            "/Applications/A&B/AskHuman",
            "/Users/test/log<1>",
            DAEMON_LAUNCHD_LABEL,
            "        <key>ASKHUMAN_HOME</key>\n        <string>/tmp/dev</string>\n",
        );

        assert!(plist.contains(&format!("<string>{DAEMON_LAUNCHD_LABEL}</string>")));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<string>Interactive</string>"));
        assert!(plist.contains("/Applications/A&amp;B/AskHuman"));
        assert!(plist.contains("/Users/test/log&lt;1&gt;"));
        assert!(plist.contains("<key>ASKHUMAN_HOME</key>"));
        assert!(!plist.contains("<key>KeepAlive</key>"));
    }
}
