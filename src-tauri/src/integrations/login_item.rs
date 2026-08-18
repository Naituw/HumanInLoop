//! 开机自启「登录项」集成（spec D12，仅 `menuBarIcon = always`）。
//!
//! 切到 always 时安装登录项，使「重启系统后 / daemon 没起时」菜单栏图标也一直在；切走时移除。
//! - macOS：`~/Library/LaunchAgents/<id>.guihost.plist`（`RunAtLoad` + `KeepAlive`）。
//!   `KeepAlive` 兼作宿主二进制换新的守护——宿主退出后由 launchd 用**新二进制**重启。
//! - Linux：`~/.config/autostart/askhuman-guihost.desktop`（`X-GNOME-Autostart-enabled=true`）。
//! - Windows：当前用户 Run registry 指向受管 wscript.exe launcher；launcher 再以 hidden
//!   window style 启动 console-subsystem EXE，登录时不闪控制台。
//!
//! 全部 best-effort：写文件 + 尽力 load/unload；失败不阻塞模式切换（图标仍可由 daemon 兜底拉起）。

#[cfg(unix)]
use std::path::PathBuf;

/// LaunchAgent / autostart 的标识（基于 bundle id 派生）。
const LABEL: &str = "com.naituw.humaninloop.guihost";

#[cfg(windows)]
const WINDOWS_GUI_VALUE: &str = "AskHuman GUI Host";

#[cfg(windows)]
const WINDOWS_DAEMON_VALUE: &str = "AskHuman Daemon";

/// 当前可执行文件路径（解析失败回退到字面名，仅用于内容生成）。
fn current_exe() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "AskHuman".to_string())
}

/// Dev Instance（隔离实例）上下文：**绝不**读写用户级全局登录项。
/// LaunchAgents / autostart 的 label 全用户唯一，dev 实例写入会把生产的开机自启劫持到
/// worktree 二进制（launchd 用无 `ASKHUMAN_HOME` 的环境重启它 → 以生产 home 运行外来构建，
/// 生产托盘/窗口被旧代码接管；卸载路径则会误删生产登录项）。用户实证 2026-07-25：并行
/// worktree 实例反复劫持导致控制台无限 Loading。判定用 env + exe 路径双料（launchd 重启的
/// 进程无 env，靠 `.askhuman-dev` 路径段兜底）。
fn is_dev_instance_context() -> bool {
    if std::env::var(crate::dev_instance::ASKHUMAN_HOME_ENV).is_ok_and(|v| !v.trim().is_empty()) {
        return true;
    }
    std::env::current_exe().is_ok_and(|p| {
        p.components()
            .any(|c| c.as_os_str() == crate::dev_instance::DEV_DIR)
    })
}

// ===== Windows: per-user Run registry values =====

#[cfg(windows)]
mod windows_run {
    use std::io;
    use std::ptr;
    use windows_sys::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, REG_SZ,
    };

    const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            unsafe {
                RegCloseKey(self.0);
            }
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn status_result(status: u32) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(status as i32))
        }
    }

    fn open(access: u32) -> io::Result<Key> {
        let path = wide(RUN_KEY);
        let mut key = ptr::null_mut();
        let status =
            unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), 0, access, &mut key) };
        status_result(status)?;
        Ok(Key(key))
    }

    fn create() -> io::Result<Key> {
        let path = wide(RUN_KEY);
        let mut key = ptr::null_mut();
        let status = unsafe { RegCreateKeyW(HKEY_CURRENT_USER, path.as_ptr(), &mut key) };
        status_result(status)?;
        Ok(Key(key))
    }

    pub fn read(name: &str) -> io::Result<Option<String>> {
        let key = match open(KEY_READ) {
            Ok(key) => key,
            Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                return Ok(None)
            }
            Err(error) => return Err(error),
        };
        let name = wide(name);
        let mut kind = 0;
        let mut bytes = 0;
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                ptr::null_mut(),
                &mut bytes,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        status_result(status)?;
        if kind != REG_SZ {
            return Ok(None);
        }
        let mut data = vec![0u16; (bytes as usize).div_ceil(2).max(1)];
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                data.as_mut_ptr().cast(),
                &mut bytes,
            )
        };
        status_result(status)?;
        let length = data.iter().position(|ch| *ch == 0).unwrap_or(data.len());
        Ok(Some(String::from_utf16_lossy(&data[..length])))
    }

    pub fn write(name: &str, value: &str) -> io::Result<()> {
        let key = create()?;
        let name = wide(name);
        let value = wide(value);
        let bytes = value
            .len()
            .checked_mul(2)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Run value is too large"))?;
        let status = unsafe {
            RegSetValueExW(
                key.0,
                name.as_ptr(),
                0,
                REG_SZ,
                value.as_ptr().cast(),
                bytes,
            )
        };
        status_result(status)
    }

    pub fn remove(name: &str) -> io::Result<()> {
        let key = match open(KEY_SET_VALUE) {
            Ok(key) => key,
            Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => {
                return Ok(())
            }
            Err(error) => return Err(error),
        };
        let name = wide(name);
        let status = unsafe { RegDeleteValueW(key.0, name.as_ptr()) };
        if status == ERROR_FILE_NOT_FOUND {
            Ok(())
        } else {
            status_result(status)
        }
    }
}

#[cfg(windows)]
const WINDOWS_LAUNCHER_NAME: &str = "askhuman-login.vbs";

#[cfg(windows)]
fn windows_launcher_path() -> std::path::PathBuf {
    crate::paths::config_dir().join(WINDOWS_LAUNCHER_NAME)
}

#[cfg(windows)]
fn windows_script_host() -> std::path::PathBuf {
    std::env::var_os("WINDIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("wscript.exe")
}

#[cfg(windows)]
fn windows_launcher_contents(exe: &str) -> String {
    let exe = exe.replace('"', "\"\"");
    format!(
        "Option Explicit\r\n\
Dim mode, command, shell\r\n\
If WScript.Arguments.Count <> 1 Then WScript.Quit 2\r\n\
mode = LCase(WScript.Arguments(0))\r\n\
If mode = \"gui-host\" Then\r\n\
  command = \"\"\"{exe}\"\" --gui-host\"\r\n\
ElseIf mode = \"daemon\" Then\r\n\
  command = \"\"\"{exe}\"\" daemon run\"\r\n\
Else\r\n\
  WScript.Quit 2\r\n\
End If\r\n\
Set shell = CreateObject(\"WScript.Shell\")\r\n\
shell.Run command, 0, False\r\n"
    )
}

#[cfg(windows)]
fn windows_launcher_bytes(exe: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in windows_launcher_contents(exe).encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

#[cfg(windows)]
fn windows_launcher_is_current() -> bool {
    std::fs::read(windows_launcher_path())
        .is_ok_and(|installed| installed == windows_launcher_bytes(&current_exe()))
}

#[cfg(windows)]
fn ensure_windows_launcher() -> std::io::Result<()> {
    let path = windows_launcher_path();
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Windows login launcher has no parent directory",
        )
    })?;
    std::fs::create_dir_all(parent)?;
    let bytes = windows_launcher_bytes(&current_exe());
    if std::fs::read(&path).is_ok_and(|installed| installed == bytes) {
        return Ok(());
    }
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, &path)?;
    Ok(())
}

#[cfg(windows)]
fn windows_command(role: &str) -> String {
    format!(
        r#""{}" //B //NoLogo "{}" {}"#,
        windows_script_host().display(),
        windows_launcher_path().display(),
        role
    )
}

#[cfg(windows)]
fn remove_windows_launcher_if_unused() -> std::io::Result<()> {
    let gui_absent = windows_run::read(WINDOWS_GUI_VALUE)?.is_none();
    let daemon_absent = windows_run::read(WINDOWS_DAEMON_VALUE)?.is_none();
    let path = windows_launcher_path();
    if gui_absent && daemon_absent && path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn install() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(());
    }
    ensure_windows_launcher()?;
    windows_run::write(WINDOWS_GUI_VALUE, &windows_command("gui-host"))
}

#[cfg(windows)]
pub fn uninstall() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(());
    }
    windows_run::remove(WINDOWS_GUI_VALUE)?;
    remove_windows_launcher_if_unused()
}

// ===== macOS：LaunchAgent plist =====

#[cfg(target_os = "macos")]
fn item_path() -> PathBuf {
    crate::paths::home()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{LABEL}.plist"))
}

/// 生成 LaunchAgent plist 内容（纯函数，便于单测）。
#[cfg(target_os = "macos")]
fn plist_contents(exe: &str) -> String {
    // 简单 XML 转义（路径理论上可能含 & < >）。
    let exe = exe
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>--gui-host</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#
    )
}

#[cfg(target_os = "macos")]
pub fn install() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    let path = item_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, plist_contents(&current_exe()))?;
    // 先 bootout 旧实例（忽略错误），再 bootstrap 新的；失败回退 load -w。best-effort。
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let _ = run(
        "launchctl",
        &["bootout", &domain, &path.display().to_string()],
    );
    if run(
        "launchctl",
        &["bootstrap", &domain, &path.display().to_string()],
    )
    .is_err()
    {
        let _ = run("launchctl", &["load", "-w", &path.display().to_string()]);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    let path = item_path();
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let _ = run(
        "launchctl",
        &["bootout", &domain, &path.display().to_string()],
    );
    let _ = run("launchctl", &["unload", &path.display().to_string()]);
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

// ===== Linux：autostart .desktop =====

#[cfg(all(unix, not(target_os = "macos")))]
fn item_path() -> PathBuf {
    crate::paths::home()
        .join(".config")
        .join("autostart")
        .join("askhuman-guihost.desktop")
}

/// 生成 autostart .desktop 内容（纯函数，便于单测）。
#[cfg(all(unix, not(target_os = "macos")))]
fn desktop_contents(exe: &str) -> String {
    format!(
        "[Desktop Entry]\n\
Type=Application\n\
Name=AskHuman Menu Bar\n\
Exec=\"{exe}\" --gui-host\n\
X-GNOME-Autostart-enabled=true\n\
NoDisplay=true\n\
Terminal=false\n"
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn install() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    let path = item_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, desktop_contents(&current_exe()))
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn uninstall() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    let path = item_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

// ===== Daemon 登录项（保活模式：daemon 本体开机自启）=====
//
// 与 guihost 登录项**分开**，且**纯文件写/删**（不 launchctl bootstrap/bootout）。理由：
// - 保活模式下 daemon 由「打开开关即 `client::ensure_running`」在当前会话即起；登录项只负责**下次登录**自启
//   （~/Library/LaunchAgents 下的 plist 会在登录时被 launchd 自动加载，无需显式 bootstrap）。
// - 关闭保活时只删文件、**绝不 bootout**：bootout 会给正在跑的 daemon 发 SIGTERM 强杀，而需求是让它按
//   原 5min 空闲策略自然退出。故避免任何会杀进程的 launchctl 操作。
// - KeepAlive=false：避免「daemon 空闲退出后被 launchd 立刻拉起」与自然退出/换挡打架。
// - macOS 必须用 Interactive：daemon 会直接 spawn GUI popup helper；若自身是 Background，
//   helper 会继承后台调度角色（即使窗口已显示/聚焦仍是低优先级），造成整窗交互持续掉帧。

const DAEMON_LABEL: &str = "com.naituw.humaninloop.daemon";

#[cfg(target_os = "macos")]
fn daemon_item_path() -> PathBuf {
    crate::paths::home()
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{DAEMON_LABEL}.plist"))
}

#[cfg(target_os = "macos")]
fn daemon_contents(exe: &str) -> String {
    let exe = exe
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{DAEMON_LABEL}</string>
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
</dict>
</plist>
"#
    )
}

#[cfg(all(unix, not(target_os = "macos")))]
fn daemon_item_path() -> PathBuf {
    crate::paths::home()
        .join(".config")
        .join("autostart")
        .join("askhuman-daemon.desktop")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn daemon_contents(exe: &str) -> String {
    format!(
        "[Desktop Entry]\n\
Type=Application\n\
Name=AskHuman Daemon\n\
Exec=\"{exe}\" daemon start\n\
X-GNOME-Autostart-enabled=true\n\
NoDisplay=true\n\
Terminal=false\n"
    )
}

/// daemon 登录项是否已安装。
#[cfg(unix)]
pub fn daemon_is_installed() -> bool {
    daemon_item_path().exists()
}

/// 已装模板与当前期望不一致（移动安装位置或模板升级后需刷新）。
#[cfg(unix)]
pub fn daemon_needs_update() -> bool {
    if !daemon_is_installed() {
        return false;
    }
    match std::fs::read_to_string(daemon_item_path()) {
        Ok(text) => daemon_template_needs_update(&text, &current_exe()),
        Err(_) => true,
    }
}

/// daemon 登录项是完全托管文件，逐字比较可同时发现 exe 迁移与模板语义升级
/// （例如旧版 macOS plist 的 Background → Interactive）。
#[cfg(unix)]
fn daemon_template_needs_update(installed: &str, exe: &str) -> bool {
    installed != daemon_contents(exe)
}

/// 写入/刷新 daemon 登录项文件（纯文件、不 launchctl）。幂等。
#[cfg(unix)]
pub fn install_daemon() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    let path = daemon_item_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, daemon_contents(&current_exe()))
}

/// 删除 daemon 登录项文件（**不** bootout，避免强杀正在运行的 daemon）。幂等。
#[cfg(unix)]
pub fn uninstall_daemon() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    let path = daemon_item_path();
    if path.exists() {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

#[cfg(windows)]
pub fn daemon_is_installed() -> bool {
    windows_run::read(WINDOWS_DAEMON_VALUE).is_ok_and(|value| value.is_some())
}

#[cfg(windows)]
pub fn daemon_needs_update() -> bool {
    match windows_run::read(WINDOWS_DAEMON_VALUE) {
        Ok(Some(installed)) => {
            installed != windows_command("daemon") || !windows_launcher_is_current()
        }
        Ok(None) => false,
        Err(_) => true,
    }
}

#[cfg(windows)]
pub fn install_daemon() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(());
    }
    ensure_windows_launcher()?;
    windows_run::write(WINDOWS_DAEMON_VALUE, &windows_command("daemon"))
}

#[cfg(windows)]
pub fn uninstall_daemon() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(());
    }
    windows_run::remove(WINDOWS_DAEMON_VALUE)?;
    remove_windows_launcher_if_unused()
}

/// 让 daemon 登录项与「是否保活」一致：保活→写/刷新文件、否则→删文件。幂等，供 daemon 启动 /
/// 配置变更 / 宿主换挡复用。
pub fn sync_daemon(keep_alive: bool) -> std::io::Result<()> {
    if keep_alive {
        if daemon_is_installed() && !daemon_needs_update() {
            Ok(())
        } else {
            install_daemon()
        }
    } else {
        uninstall_daemon()
    }
}

// ===== 共用 =====

/// 登录项是否已安装。
#[cfg(unix)]
pub fn is_installed() -> bool {
    item_path().exists()
}

/// 已安装但记录的 exe 路径与当前不一致（移动安装位置后需刷新）。
#[cfg(unix)]
pub fn needs_update() -> bool {
    if !is_installed() {
        return false;
    }
    let exe = current_exe();
    match std::fs::read_to_string(item_path()) {
        Ok(text) => !text.contains(&exe),
        Err(_) => true,
    }
}

/// 确保登录项与当前 exe 一致：缺失或需更新则（重）安装。幂等。
pub fn ensure_installed() -> std::io::Result<()> {
    if is_dev_instance_context() {
        return Ok(()); // dev 实例不触碰全局登录项（见 is_dev_instance_context 注释）。
    }
    if !is_installed() || needs_update() {
        install()
    } else {
        Ok(())
    }
}

#[cfg(target_os = "macos")]
fn run(cmd: &str, args: &[&str]) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let status = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("{cmd} exited with {status}")))
    }
}

#[cfg(windows)]
pub fn is_installed() -> bool {
    windows_run::read(WINDOWS_GUI_VALUE).is_ok_and(|value| value.is_some())
}

#[cfg(windows)]
pub fn needs_update() -> bool {
    match windows_run::read(WINDOWS_GUI_VALUE) {
        Ok(Some(installed)) => {
            installed != windows_command("gui-host") || !windows_launcher_is_current()
        }
        Ok(None) => false,
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_has_args_and_keepalive() {
        let p = plist_contents("/usr/local/bin/AskHuman");
        assert!(p.contains("<string>/usr/local/bin/AskHuman</string>"));
        assert!(p.contains("<string>--gui-host</string>"));
        assert!(p.contains("<key>KeepAlive</key>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        assert!(p.contains(LABEL));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn desktop_has_exec_and_autostart() {
        let d = desktop_contents("/home/u/.local/bin/AskHuman");
        assert!(d.contains("Exec=\"/home/u/.local/bin/AskHuman\" --gui-host"));
        assert!(d.contains("X-GNOME-Autostart-enabled=true"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn daemon_plist_runs_daemon_at_load_without_keepalive() {
        let p = daemon_contents("/usr/local/bin/AskHuman");
        assert!(p.contains("<string>/usr/local/bin/AskHuman</string>"));
        assert!(p.contains("<string>daemon</string>"));
        assert!(p.contains("<string>run</string>"));
        assert!(p.contains("<key>RunAtLoad</key>"));
        // 保活 daemon 登录项刻意**不带** KeepAlive（见模块头注释）。
        assert!(!p.contains("<key>KeepAlive</key>"));
        // daemon 会 spawn GUI helper，不能用 Background（子进程会继承后台低优先级）。
        assert!(p.contains("<string>Interactive</string>"));
        assert!(!p.contains("<string>Background</string>"));
        assert!(p.contains(DAEMON_LABEL));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn daemon_template_refreshes_legacy_background_plist() {
        let exe = "/usr/local/bin/AskHuman";
        let current = daemon_contents(exe);
        assert!(!daemon_template_needs_update(&current, exe));

        let legacy = current.replace(
            "<string>Interactive</string>",
            "<string>Background</string>",
        );
        assert!(daemon_template_needs_update(&legacy, exe));
        assert!(daemon_template_needs_update(
            &current,
            "/opt/askhuman/AskHuman"
        ));
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn daemon_desktop_starts_daemon() {
        let d = daemon_contents("/home/u/.local/bin/AskHuman");
        assert!(d.contains("Exec=\"/home/u/.local/bin/AskHuman\" daemon start"));
        assert!(d.contains("X-GNOME-Autostart-enabled=true"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_login_commands_use_managed_hidden_launcher() {
        let gui = windows_command("gui-host");
        let daemon = windows_command("daemon");
        assert!(gui
            .to_ascii_lowercase()
            .contains("wscript.exe\" //b //nologo"));
        assert!(gui.ends_with("askhuman-login.vbs\" gui-host"));
        assert!(daemon.ends_with("askhuman-login.vbs\" daemon"));

        let script = windows_launcher_contents(r"C:\Program Files\AskHuman\AskHuman.exe");
        assert!(script.contains(
            "command = \"\"\"C:\\Program Files\\AskHuman\\AskHuman.exe\"\" --gui-host\""
        ));
        assert!(script.contains("shell.Run command, 0, False"));
    }

    /// Dev 实例上下文判定（防生产登录项劫持，用户实证 2026-07-25）：`ASKHUMAN_HOME` 置位
    /// 即视为隔离实例。exe 路径分支（launchd 重启无 env 的兜底）无法在测试内伪造，仅测 env 分支。
    #[test]
    fn dev_instance_context_follows_home_env() {
        // 与 paths.rs 的 env 测试同约定：串行修改进程 env，结束后恢复。
        let key = crate::dev_instance::ASKHUMAN_HOME_ENV;
        let prev = std::env::var_os(key);
        std::env::set_var(key, "/tmp/x/.askhuman-dev/home");
        assert!(is_dev_instance_context());
        std::env::remove_var(key);
        // 无 env 时结果取决于测试二进制路径（target/ 下不含 .askhuman-dev）→ false。
        assert!(!is_dev_instance_context());
        match prev {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}
