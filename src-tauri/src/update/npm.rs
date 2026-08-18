//! NpmUpdater：npm 全局安装的更新器。
//!
//! 检查：npm registry 的 `latest` 元数据（HTTP，不依赖本地 npm，保证「检查」始终可用）。
//! 应用：跑 `npm i -g askhuman@latest`；npm 不可用 / 执行失败 → 返回带手动命令的错误，
//! 由前端回显命令让用户手动执行。日志（notes）仍按 tag 从 GitHub 取（best-effort）。
//! **不 restart**：npm 替换 node_modules 内二进制后，daemon drain 在答完后换新。

use super::{http_client, NPM_PACKAGE};
use super::{ProgressCb, RemoteLatest, Updater};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

pub struct NpmUpdater;

impl NpmUpdater {
    pub fn new() -> Self {
        Self
    }

    /// 手动更新命令提示（npm 不可用时回显给用户）。
    pub fn manual_command() -> String {
        format!("npm i -g {}@latest", NPM_PACKAGE)
    }
}

#[async_trait::async_trait]
impl Updater for NpmUpdater {
    async fn check_latest(&self, fresh: bool) -> Result<RemoteLatest> {
        let version = npm_latest_version(fresh).await?;
        // 日志按 tag 从 GitHub 取（best-effort，取不到则空，前端显示占位）。
        let notes = super::notes::notes_for_tag(&version)
            .await
            .unwrap_or_default();
        Ok(RemoteLatest {
            source_url: super::release_url(&version),
            version,
            notes,
        })
    }

    async fn apply(&self, _progress: Option<ProgressCb>) -> Result<()> {
        #[cfg(windows)]
        {
            super::ensure_automatic_apply_allowed()?;
            stage_windows_npm_worker().await
        }
        #[cfg(not(windows))]
        {
            run_npm_install().await
        }
    }
}

/// 从 npm registry 取主包最新版本（`https://registry.npmjs.org/<pkg>/latest`）。
async fn npm_latest_version(fresh: bool) -> Result<String> {
    let url = format!("https://registry.npmjs.org/{}/latest", NPM_PACKAGE);
    let mut request = http_client().get(url).header("Accept", "application/json");
    if fresh {
        request = request
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .header(reqwest::header::PRAGMA, "no-cache");
    }
    let resp = request.send().await.context("npm registry 请求失败")?;
    if !resp.status().is_success() {
        return Err(anyhow!("npm registry 返回 {}", resp.status()));
    }
    let meta = resp.json::<Value>().await.context("解析 npm 元数据失败")?;
    let v = super::normalize_version(meta["version"].as_str().unwrap_or(""));
    if v.is_empty() {
        return Err(anyhow!("无法解析 npm 最新版本号"));
    }
    Ok(v)
}

/// 执行 `npm i -g <pkg>@latest`。npm 缺失 / 失败 → Err（含手动命令提示）。
#[cfg(not(windows))]
async fn run_npm_install() -> Result<()> {
    let cmd = NpmUpdater::manual_command();
    // 在阻塞线程跑外部命令，避免占用 async 执行器。
    let result = tokio::task::spawn_blocking(run_npm_install_blocking)
        .await
        .context("等待 npm 进程失败")?;

    match result {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(anyhow!(
            "npm 更新失败，请手动执行：{cmd}\n{}",
            String::from_utf8_lossy(&out.stderr)
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(anyhow!("未找到 npm，请手动执行：{cmd}"))
        }
        Err(e) => Err(anyhow!("无法启动 npm（{e}），请手动执行：{cmd}")),
    }
}

#[derive(Debug)]
struct NpmLaunch {
    program: OsString,
    path: Option<OsString>,
    from_install_prefix: bool,
}

/// 执行 npm。优先使用安装当前 AskHuman 的同一 npm prefix，避免 GUI / launchd 未加载
/// nvm、fnm、asdf、mise 等 shell 初始化脚本时，PATH 中找不到 npm。
#[cfg(not(windows))]
fn run_npm_install_blocking() -> std::io::Result<std::process::Output> {
    let exe = std::env::current_exe().ok();
    let current_path = std::env::var_os("PATH");
    let launch = npm_launch(exe.as_deref(), current_path.as_deref());
    let result = run_npm_command(&launch);

    // 候选在检查后可能被并发更新移走；仅这种“无法启动”场景再退回原 PATH。
    // npm 已成功启动但自身返回失败时必须保留其真实错误，不能换另一套 npm 重试。
    if launch.from_install_prefix
        && matches!(&result, Err(e) if e.kind() == std::io::ErrorKind::NotFound)
    {
        run_npm_command(&NpmLaunch {
            program: OsString::from("npm"),
            path: None,
            from_install_prefix: false,
        })
    } else {
        result
    }
}

fn run_npm_command(launch: &NpmLaunch) -> std::io::Result<std::process::Output> {
    let mut command = std::process::Command::new(&launch.program);
    command.args(["i", "-g", &format!("{}@latest", NPM_PACKAGE)]);
    if let Some(path) = &launch.path {
        command.env("PATH", path);
    }
    command.output()
}

/// 选择 npm 启动方式。Unix npm 全局包通常位于 `<prefix>/lib/node_modules`，而 npm/node
/// 位于 `<prefix>/bin`；从当前平台子包二进制向上找到这个 prefix，比依赖 GUI 进程 PATH 稳定。
fn npm_launch(exe: Option<&Path>, current_path: Option<&OsStr>) -> NpmLaunch {
    #[cfg(unix)]
    if let Some(prefix_bin) = exe.and_then(npm_prefix_bin_from_exe) {
        let npm = prefix_bin.join("npm");
        if npm.is_file() {
            return NpmLaunch {
                program: npm.into_os_string(),
                // npm 常以 `#!/usr/bin/env node` 启动；前置同 prefix 的 bin，保证它找到配套 Node。
                path: Some(prepend_path(&prefix_bin, current_path)),
                from_install_prefix: true,
            };
        }
    }
    #[cfg(windows)]
    if let Some(prefix) = exe.and_then(npm_prefix_bin_from_exe_windows) {
        let npm = prefix.join("npm.cmd");
        if npm.is_file() {
            return NpmLaunch {
                program: npm.into_os_string(),
                path: Some(prepend_path_cross_platform(&prefix, current_path)),
                from_install_prefix: true,
            };
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (exe, current_path);

    NpmLaunch {
        program: OsString::from("npm"),
        path: None,
        from_install_prefix: false,
    }
}

#[cfg(windows)]
fn npm_prefix_bin_from_exe_windows(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find_map(|directory| {
        if !directory
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case("node_modules"))
        {
            return None;
        }
        let prefix = directory.parent()?;
        prefix
            .join("npm.cmd")
            .is_file()
            .then(|| prefix.to_path_buf())
    })
}

#[cfg(windows)]
fn prepend_path_cross_platform(prefix: &Path, current_path: Option<&OsStr>) -> OsString {
    let mut entries = vec![prefix.to_path_buf()];
    if let Some(path) = current_path {
        entries.extend(std::env::split_paths(path));
    }
    std::env::join_paths(entries).unwrap_or_else(|_| prefix.as_os_str().to_os_string())
}

#[cfg(windows)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsNpmTransaction {
    target: PathBuf,
    worker: PathBuf,
    expected_version: String,
    restart_daemon: bool,
    restart_gui_host: bool,
    parent_pid: u32,
}

#[cfg(windows)]
async fn stage_windows_npm_worker() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    super::cleanup_stale_windows_workdirs();
    let target = std::env::current_exe().context("failed to locate current AskHuman.exe")?;
    let expected_version = npm_latest_version(true).await?;
    let work = std::env::temp_dir().join(format!("askhuman_npm_update_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work)?;
    let worker = work.join("AskHuman-npm-update-worker.exe");
    std::fs::copy(&target, &worker).context("failed to stage npm update worker")?;
    let transaction = WindowsNpmTransaction {
        target,
        worker: worker.clone(),
        expected_version,
        restart_daemon: crate::ipc::transport::connect().await.is_ok(),
        restart_gui_host: crate::ipc::transport::connect_role("gui-host")
            .await
            .is_ok(),
        parent_pid: std::process::id(),
    };
    let transaction_path = work.join("transaction.json");
    crate::integrations::hook_edit::atomic_write_private(
        &transaction_path,
        &serde_json::to_vec(&transaction)?,
    )?;
    let (worker_stdout, worker_stderr) = super::windows_worker_log_files("npm")?;
    let mut command = Command::new(&worker);
    command
        .arg("__npm-update-worker")
        .arg(&transaction_path)
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::from(worker_stdout))
        .stderr(Stdio::from(worker_stderr));
    command
        .spawn()
        .context("failed to start npm update worker")?;
    Ok(())
}

#[cfg(windows)]
pub fn run_windows_worker(args: &[String]) -> Result<()> {
    use std::process::{Command, Stdio};

    let transaction_path = args
        .first()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing npm update transaction"))?;
    let claimed = transaction_path.with_extension("claimed");
    std::fs::rename(&transaction_path, &claimed)
        .context("npm update transaction is missing or already claimed")?;
    let transaction: WindowsNpmTransaction = serde_json::from_slice(&std::fs::read(&claimed)?)?;
    if std::fs::canonicalize(std::env::current_exe()?)?
        != std::fs::canonicalize(&transaction.worker)?
    {
        return Err(anyhow!("npm update worker identity mismatch"));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let _ = crate::client::request_stop(false).await;
        crate::client::wait_until_down(std::time::Duration::from_secs(24 * 60 * 60)).await;
        let _ = crate::gui_host::shutdown_if_running().await;
    });
    let parent_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while crate::agents::detect::pid_alive(transaction.parent_pid)
        && std::time::Instant::now() < parent_deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if crate::agents::detect::pid_alive(transaction.parent_pid) {
        return Err(anyhow!("npm update caller did not exit"));
    }

    let launch = npm_launch(
        Some(&transaction.target),
        std::env::var_os("PATH").as_deref(),
    );
    let output = run_npm_command(&launch).context("failed to start npm update")?;
    let verified = output.status.success()
        && crate::update::direct::verify_windows_authenticode(&transaction.target).is_ok()
        && Command::new(&transaction.target)
            .arg("--version")
            .stdin(Stdio::null())
            .output()
            .is_ok_and(|output| {
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains(&transaction.expected_version)
            });
    if !verified {
        if let Some(parent) = transaction.target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(&transaction.worker, &transaction.target);
        return Err(anyhow!(
            "npm update failed or produced an invalid binary; restored the previous executable: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    if transaction.restart_daemon {
        let _ = Command::new(&transaction.target)
            .args(["daemon", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if transaction.restart_gui_host {
        let mut command = Command::new(&transaction.target);
        command
            .arg("--gui-host")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::daemon::spawn::configure_background(&mut command);
        let _ = command.spawn();
    }
    let _ = std::fs::remove_file(claimed);
    Ok(())
}

#[cfg(not(windows))]
pub fn run_windows_worker(_args: &[String]) -> Result<()> {
    Err(anyhow!(
        "Windows npm update worker is unavailable on this platform"
    ))
}

/// 从 `.../<prefix>/lib/node_modules/.../AskHuman` 反推 `<prefix>/bin`。
/// 路径内可能有两层 node_modules（主包 + 平台 optionalDependency），因此必须一直向外找
/// 到父目录名确为 `lib` 的那一层。
#[cfg(unix)]
fn npm_prefix_bin_from_exe(exe: &Path) -> Option<PathBuf> {
    exe.ancestors().find_map(|dir| {
        if dir.file_name() != Some(OsStr::new("node_modules")) {
            return None;
        }
        let lib = dir.parent()?;
        if lib.file_name() != Some(OsStr::new("lib")) {
            return None;
        }
        Some(lib.parent()?.join("bin"))
    })
}

#[cfg(unix)]
fn prepend_path(prefix_bin: &Path, current_path: Option<&OsStr>) -> OsString {
    let mut entries = vec![prefix_bin.to_path_buf()];
    if let Some(path) = current_path {
        entries.extend(std::env::split_paths(path));
    }
    std::env::join_paths(entries).unwrap_or_else(|_| prefix_bin.as_os_str().to_os_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[tokio::test]
    async fn unsigned_windows_apply_is_blocked_before_npm_staging() {
        let error = NpmUpdater::new().apply(None).await.unwrap_err();
        assert!(error.to_string().contains("automatic update"));
    }

    #[cfg(unix)]
    #[test]
    fn derives_nvm_prefix_through_nested_platform_package() {
        let exe = Path::new(
            "/Users/u/.nvm/versions/node/v22.17.0/lib/node_modules/askhuman/node_modules/\
             @humaninloop/darwin-arm64/bin/AskHuman",
        );
        assert_eq!(
            npm_prefix_bin_from_exe(exe),
            Some(PathBuf::from("/Users/u/.nvm/versions/node/v22.17.0/bin"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn derives_homebrew_and_linux_prefixes() {
        assert_eq!(
            npm_prefix_bin_from_exe(Path::new(
                "/opt/homebrew/lib/node_modules/askhuman/node_modules/\
                 @humaninloop/darwin-arm64/bin/AskHuman"
            )),
            Some(PathBuf::from("/opt/homebrew/bin"))
        );
        assert_eq!(
            npm_prefix_bin_from_exe(Path::new(
                "/usr/lib/node_modules/askhuman/node_modules/\
                 @humaninloop/linux-x64/bin/AskHuman"
            )),
            Some(PathBuf::from("/usr/bin"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn launch_prefers_prefix_npm_and_prepends_its_bin_to_path() {
        let temp = tempfile::tempdir().unwrap();
        let prefix = temp.path().join("versions/node/v22.17.0");
        let prefix_bin = prefix.join("bin");
        std::fs::create_dir_all(&prefix_bin).unwrap();
        std::fs::write(prefix_bin.join("npm"), "#!/bin/sh\n").unwrap();
        let exe = prefix
            .join("lib/node_modules/askhuman/node_modules/@humaninloop/darwin-arm64/bin/AskHuman");
        let original =
            std::env::join_paths([Path::new("/usr/local/bin"), Path::new("/usr/bin")]).unwrap();

        let launch = npm_launch(Some(&exe), Some(&original));

        assert_eq!(launch.program, prefix_bin.join("npm").into_os_string());
        assert!(launch.from_install_prefix);
        let entries: Vec<_> = std::env::split_paths(launch.path.as_ref().unwrap()).collect();
        assert_eq!(entries[0], prefix_bin);
        assert_eq!(entries[1], PathBuf::from("/usr/local/bin"));
        assert_eq!(entries[2], PathBuf::from("/usr/bin"));
    }

    #[test]
    fn launch_falls_back_to_inherited_path_when_prefix_npm_is_missing() {
        let launch = npm_launch(
            Some(Path::new(
                "/unknown/lib/node_modules/askhuman/node_modules/pkg/bin/AskHuman",
            )),
            Some(OsStr::new("/usr/bin")),
        );
        assert_eq!(launch.program, OsString::from("npm"));
        assert!(launch.path.is_none());
        assert!(!launch.from_install_prefix);
    }
}
