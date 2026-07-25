//! 开机自启「登录项」集成（spec D12，仅 `menuBarIcon = always`）。
//!
//! 切到 always 时安装登录项，使「重启系统后 / daemon 没起时」菜单栏图标也一直在；切走时移除。
//! - macOS：`~/Library/LaunchAgents/<id>.guihost.plist`（`RunAtLoad` + `KeepAlive`）。
//!   `KeepAlive` 兼作宿主二进制换新的守护——宿主退出后由 launchd 用**新二进制**重启。
//! - Linux：`~/.config/autostart/askhuman-guihost.desktop`（`X-GNOME-Autostart-enabled=true`）。
//!
//! 全部 best-effort：写文件 + 尽力 load/unload；失败不阻塞模式切换（图标仍可由 daemon 兜底拉起）。

#![cfg(unix)]

use std::path::PathBuf;

/// LaunchAgent / autostart 的标识（基于 bundle id 派生）。
const LABEL: &str = "com.naituw.humaninloop.guihost";

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
pub fn daemon_is_installed() -> bool {
    daemon_item_path().exists()
}

/// 已装模板与当前期望不一致（移动安装位置或模板升级后需刷新）。
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
fn daemon_template_needs_update(installed: &str, exe: &str) -> bool {
    installed != daemon_contents(exe)
}

/// 写入/刷新 daemon 登录项文件（纯文件、不 launchctl）。幂等。
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
pub fn is_installed() -> bool {
    item_path().exists()
}

/// 已安装但记录的 exe 路径与当前不一致（移动安装位置后需刷新）。
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
