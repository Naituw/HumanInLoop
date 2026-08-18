//! DirectUpdater：直装二进制（install.sh / 手动下载）的更新器。
//!
//! 检查：GitHub Releases `/releases/latest`。
//! 应用：下载平台资产（`AskHuman-<目标三元组>-v<版本>.{tar.gz,zip}`）→ 解压 →
//! 找可执行文件 → (macOS) 校验 Developer ID/TeamID → 备份当前 `.bak` → 原子替换
//! `current_exe`。**不 restart**：盘上一变，daemon drain 在答完后换新。
//!
//! 移植自参考实现 `humanInLoop-rust/src/rust/ui/updater.rs`，去掉其 `app.restart()`。

use super::{
    github_api_url, http_client, target_triple, DownloadProgress, ProgressCb, RemoteLatest, Updater,
};
use anyhow::{anyhow, Context, Result};
use serde_json::Value;

pub struct DirectUpdater;

impl DirectUpdater {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl Updater for DirectUpdater {
    async fn check_latest(&self, fresh: bool) -> Result<RemoteLatest> {
        let release = fetch_latest_release(fresh).await?;
        let version = super::normalize_version(release["tag_name"].as_str().unwrap_or(""));
        if version.is_empty() {
            return Err(anyhow!("无法解析最新版本号"));
        }
        let notes = release["body"].as_str().unwrap_or("").to_string();
        // 优先平台资产直链；找不到回退到 release 页面（apply 时据此判定需手动下载）。
        let source_url = asset_url_for_current(&release)
            .or_else(|| release["html_url"].as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        Ok(RemoteLatest {
            version,
            notes,
            source_url,
        })
    }

    async fn apply(&self, progress: Option<ProgressCb>) -> Result<()> {
        #[cfg(unix)]
        {
            apply_unix(progress).await
        }
        #[cfg(windows)]
        {
            super::ensure_automatic_apply_allowed()?;
            apply_windows(progress).await
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = progress;
            Err(anyhow!("auto-update is unsupported on this platform"))
        }
    }
}

/// 拉取 `/releases/latest` 的 JSON（用带 token 的 GitHub API 客户端）。
async fn fetch_latest_release(fresh: bool) -> Result<Value> {
    let mut request = super::github_client()
        .get(github_api_url("releases/latest"))
        .header("Accept", "application/vnd.github+json");
    if fresh {
        request = request
            .header(reqwest::header::CACHE_CONTROL, "no-cache")
            .header(reqwest::header::PRAGMA, "no-cache");
    }
    let resp = request.send().await.context("GitHub API 请求失败")?;
    if !resp.status().is_success() {
        return Err(super::github_status_error(resp.status()));
    }
    resp.json::<Value>().await.context("解析 release JSON 失败")
}

/// 在 release.assets 里找当前平台的资产下载直链（按目标三元组匹配）。
fn asset_url_for_current(release: &Value) -> Option<String> {
    let triple = target_triple()?;
    asset_url_for_triple(release, triple)
}

/// 纯函数：按目标三元组在 assets 数组里找 `browser_download_url`（便于单测）。
fn asset_url_for_triple(release: &Value, triple: &str) -> Option<String> {
    let assets = release.get("assets")?.as_array()?;
    for a in assets {
        let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
        if name.contains(triple) {
            if let Some(url) = a.get("browser_download_url").and_then(|v| v.as_str()) {
                return Some(url.to_string());
            }
        }
    }
    None
}

/// 纯函数：按资产名精确匹配 `browser_download_url`。
fn asset_url_by_name(release: &Value, name: &str) -> Option<String> {
    let assets = release.get("assets")?.as_array()?;
    assets
        .iter()
        .find(|a| a.get("name").and_then(|v| v.as_str()) == Some(name))
        .and_then(|a| a.get("browser_download_url").and_then(|v| v.as_str()))
        .map(|s| s.to_string())
}

/// 下载 SHA256SUMS 并校验已下载压缩包的哈希（`sha256sum` 输出格式：`<hex>  <文件名>`）。
async fn verify_archive_sha256(
    archive: &std::path::Path,
    file_name: &str,
    sums_url: &str,
) -> Result<()> {
    use sha2::{Digest, Sha256};
    let resp = http_client()
        .get(sums_url)
        .send()
        .await
        .context("下载 SHA256SUMS 失败")?;
    if !resp.status().is_success() {
        return Err(anyhow!("下载 SHA256SUMS 失败：HTTP {}", resp.status()));
    }
    let text = resp.text().await.context("读取 SHA256SUMS 失败")?;
    let expected = expected_sha256(&text, file_name)
        .ok_or_else(|| anyhow!("SHA256SUMS 中没有 {file_name} 的条目"))?;
    let bytes = std::fs::read(archive).context("读取下载文件失败")?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if !actual.eq_ignore_ascii_case(&expected) {
        return Err(anyhow!(
            "下载文件校验和不符（期望 {expected}，实际 {actual}），已中止更新"
        ));
    }
    Ok(())
}

/// 纯函数：从 `sha256sum` 格式文本中取指定文件的哈希（容忍 `*` 二进制标记）。
fn expected_sha256(sums: &str, file_name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim().split_once(char::is_whitespace)?;
        let name = name.trim().trim_start_matches('*');
        (name == file_name && !hash.is_empty()).then(|| hash.to_string())
    })
}

#[cfg(unix)]
async fn apply_unix(progress: Option<ProgressCb>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    // 取新鲜的资产直链（避免用陈旧的 check 结果）。
    let release = fetch_latest_release(true).await?;
    let url = asset_url_for_current(&release)
        .ok_or_else(|| anyhow!("未找到当前平台的发布资产，请手动下载"))?;
    if url.contains("/releases/tag/") || !url.contains("://") {
        return Err(anyhow!("未找到平台预编译资产，请手动下载最新版本"));
    }

    let work = std::env::temp_dir().join(format!("askhuman_update_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).context("创建临时目录失败")?;
    // 收尾清理临时目录（best-effort）。
    let _guard = scopeguard(work.clone());

    // 下载资产到临时文件（带进度回调）。
    let file_name = url
        .rsplit('/')
        .next()
        .unwrap_or("askhuman_update")
        .to_string();
    let archive = work.join(&file_name);
    download_with_progress(&url, &archive, progress).await?;

    // 完整性校验：release 若带 SHA256SUMS（新发布均随资产发布），下载产物必须匹配；
    // 旧发布没有该文件则跳过（macOS 后续还有 codesign 把关真实性）。
    if let Some(sums_url) = asset_url_by_name(&release, "SHA256SUMS") {
        verify_archive_sha256(&archive, &file_name, &sums_url).await?;
    }

    // 解压（shell out tar/unzip；mac/Linux 自带）。
    let extract = work.join("extract");
    std::fs::create_dir_all(&extract).context("创建解压目录失败")?;
    extract_archive(&archive, &extract)?;

    // 找解压出的可执行文件 AskHuman。
    let new_exe =
        find_executable(&extract).ok_or_else(|| anyhow!("压缩包中未找到 AskHuman 可执行文件"))?;

    // macOS：替换前校验签名 + TeamID（完整性 + 钥匙串信任连续）。
    #[cfg(target_os = "macos")]
    verify_macos_signature(&new_exe)?;

    let current = std::env::current_exe().context("无法获取当前可执行文件路径")?;

    // 备份当前二进制为 <exe>.<版本>.bak（同名冲突追加序号）。
    if let Some(bak) = backup_path(&current) {
        let _ = std::fs::copy(&current, &bak);
    }

    // 原子替换：先复制到目标同目录临时文件、chmod 0755，再 rename 覆盖。
    let dir = current
        .parent()
        .ok_or_else(|| anyhow!("无法定位安装目录"))?;
    let staged = dir.join(format!(".AskHuman.new-{}", uuid::Uuid::new_v4()));
    std::fs::copy(&new_exe, &staged).context("暂存新二进制失败")?;
    let mut perms = std::fs::metadata(&staged)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&staged, perms).context("设置可执行权限失败")?;
    std::fs::rename(&staged, &current).context("替换二进制失败")?;

    Ok(())
}

#[cfg(windows)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct WindowsUpdateTransaction {
    target: std::path::PathBuf,
    worker: std::path::PathBuf,
    expected_version: String,
    #[serde(default)]
    expected_sha256: Option<String>,
    #[serde(default)]
    preserve_runtime_state: bool,
    restart_daemon: bool,
    restart_gui_host: bool,
    parent_pid: u32,
}

#[cfg(windows)]
async fn apply_windows(progress: Option<ProgressCb>) -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    super::cleanup_stale_windows_workdirs();
    let release = fetch_latest_release(true).await?;
    let expected_version = super::normalize_version(release["tag_name"].as_str().unwrap_or(""));
    let url = asset_url_for_current(&release)
        .ok_or_else(|| anyhow!("未找到当前 Windows 平台的发布资产，请手动下载"))?;
    if !url.contains("://") {
        return Err(anyhow!("发布资产地址无效"));
    }

    let work = std::env::temp_dir().join(format!("askhuman_update_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work).context("创建临时目录失败")?;
    let file_name = url
        .rsplit('/')
        .next()
        .unwrap_or("AskHuman-windows.zip")
        .to_string();
    let archive = work.join(&file_name);
    download_with_progress(&url, &archive, progress).await?;
    if let Some(sums_url) = asset_url_by_name(&release, "SHA256SUMS") {
        verify_archive_sha256(&archive, &file_name, &sums_url).await?;
    }

    let extract = work.join("extract");
    std::fs::create_dir_all(&extract).context("创建解压目录失败")?;
    extract_archive(&archive, &extract)?;
    let worker = find_executable(&extract).ok_or_else(|| anyhow!("压缩包中未找到 AskHuman.exe"))?;
    verify_windows_authenticode(&worker)?;
    let version = Command::new(&worker)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .context("无法验证下载的 AskHuman.exe")?;
    if !version.status.success()
        || !String::from_utf8_lossy(&version.stdout).contains(&expected_version)
    {
        return Err(anyhow!("下载的 AskHuman.exe 版本验证失败"));
    }

    let target = std::env::current_exe().context("无法获取当前可执行文件路径")?;
    let transaction = WindowsUpdateTransaction {
        target,
        worker: worker.clone(),
        expected_version,
        expected_sha256: file_sha256(&worker).ok(),
        preserve_runtime_state: false,
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
    let (worker_stdout, worker_stderr) = super::windows_worker_log_files("direct")?;
    let mut command = Command::new(&worker);
    command
        .arg("__update-worker")
        .arg(&transaction_path)
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::from(worker_stdout))
        .stderr(Stdio::from(worker_stderr));
    command.spawn().context("无法启动 Windows 更新 worker")?;
    Ok(())
}

/// Validate the embedded Authenticode signature and its trust chain without displaying UI.
/// The release workflow timestamps every Windows artifact, so WinVerifyTrust also validates the
/// timestamped signature when the short-lived signing certificate has expired.
#[cfg(windows)]
pub(crate) fn verify_windows_authenticode(path: &std::path::Path) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::WinTrust::{
        WinVerifyTrust, WINTRUST_ACTION_GENERIC_VERIFY_V2, WINTRUST_DATA, WINTRUST_DATA_0,
        WINTRUST_FILE_INFO, WTD_CHOICE_FILE, WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        WTD_REVOKE_WHOLECHAIN, WTD_STATEACTION_CLOSE, WTD_STATEACTION_VERIFY, WTD_UI_NONE,
    };

    let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut file = WINTRUST_FILE_INFO {
        cbStruct: std::mem::size_of::<WINTRUST_FILE_INFO>() as u32,
        pcwszFilePath: wide_path.as_ptr(),
        ..Default::default()
    };
    let mut data = WINTRUST_DATA {
        cbStruct: std::mem::size_of::<WINTRUST_DATA>() as u32,
        dwUIChoice: WTD_UI_NONE,
        fdwRevocationChecks: WTD_REVOKE_WHOLECHAIN,
        dwUnionChoice: WTD_CHOICE_FILE,
        Anonymous: WINTRUST_DATA_0 { pFile: &mut file },
        dwStateAction: WTD_STATEACTION_VERIFY,
        dwProvFlags: WTD_REVOCATION_CHECK_CHAIN_EXCLUDE_ROOT,
        ..Default::default()
    };
    let mut action = WINTRUST_ACTION_GENERIC_VERIFY_V2;
    let status = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    data.dwStateAction = WTD_STATEACTION_CLOSE;
    let _ = unsafe {
        WinVerifyTrust(
            std::ptr::null_mut(),
            &mut action,
            (&mut data as *mut WINTRUST_DATA).cast(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(anyhow!(
            "Windows Authenticode verification failed for {} (0x{:08x})",
            path.display(),
            status as u32
        ))
    }
}

/// Hidden Windows updater role. It runs from the verified new binary outside the install path,
/// drains long-lived processes, replaces the locked target transactionally, verifies it, and
/// restores the daemon/GUI Host state that existed before the update.
#[cfg(windows)]
pub fn run_windows_worker(args: &[String]) -> Result<()> {
    use std::process::{Command, Stdio};

    let transaction_path = args
        .first()
        .map(std::path::PathBuf::from)
        .ok_or_else(|| anyhow!("missing update transaction"))?;
    let claimed_path = transaction_path.with_extension("claimed");
    std::fs::rename(&transaction_path, &claimed_path)
        .context("update transaction is missing or already claimed")?;
    let transaction: WindowsUpdateTransaction =
        serde_json::from_slice(&std::fs::read(&claimed_path)?)?;
    let running_worker = std::fs::canonicalize(std::env::current_exe()?)?;
    if running_worker != std::fs::canonicalize(&transaction.worker)? {
        return Err(anyhow!("update worker identity mismatch"));
    }
    if transaction
        .target
        .file_name()
        .and_then(|name| name.to_str())
        != Some("AskHuman.exe")
    {
        return Err(anyhow!("refusing to replace an unexpected target"));
    }

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let (daemon_was_running, gui_host_was_running) = runtime.block_on(async {
        let daemon_was_running = crate::ipc::transport::connect().await.is_ok();
        let gui_host_was_running = crate::ipc::transport::connect_role("gui-host")
            .await
            .is_ok();
        let _ = crate::client::request_stop(false).await;
        crate::client::wait_until_down(std::time::Duration::from_secs(24 * 60 * 60)).await;
        let _ = crate::gui_host::shutdown_if_running().await;
        for _ in 0..300 {
            if crate::ipc::transport::connect_role("gui-host")
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        (daemon_was_running, gui_host_was_running)
    });
    let restart_daemon =
        transaction.restart_daemon || (transaction.preserve_runtime_state && daemon_was_running);
    let restart_gui_host = transaction.restart_gui_host
        || (transaction.preserve_runtime_state && gui_host_was_running);

    let parent_deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while crate::agents::detect::pid_alive(transaction.parent_pid)
        && std::time::Instant::now() < parent_deadline
    {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    if crate::agents::detect::pid_alive(transaction.parent_pid) {
        return Err(anyhow!("update caller did not exit"));
    }

    let directory = transaction
        .target
        .parent()
        .ok_or_else(|| anyhow!("update target has no parent directory"))?;
    let staged = directory.join(format!(".AskHuman.new-{}.exe", uuid::Uuid::new_v4()));
    std::fs::copy(&running_worker, &staged).context("failed to stage updated executable")?;
    let backup = if transaction.target.exists() {
        let backup = backup_path(&transaction.target)
            .ok_or_else(|| anyhow!("failed to allocate update backup path"))?;
        let replace_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10 * 60);
        loop {
            match std::fs::rename(&transaction.target, &backup) {
                Ok(()) => break,
                Err(error) if std::time::Instant::now() < replace_deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    let _ = error;
                }
                Err(error) => {
                    return Err(error).context("timed out waiting for AskHuman.exe handles");
                }
            }
        }
        Some(backup)
    } else {
        None
    };
    if let Err(error) = std::fs::rename(&staged, &transaction.target) {
        if let Some(backup) = backup.as_ref() {
            let _ = std::fs::rename(backup, &transaction.target);
        }
        return Err(error).context("failed to install updated executable; restored backup");
    }

    let verification = Command::new(&transaction.target)
        .arg("--version")
        .stdin(Stdio::null())
        .output();
    let version_verified = verification.is_ok_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).contains(&transaction.expected_version)
    });
    let hash_verified = transaction.expected_sha256.as_ref().is_none_or(|expected| {
        expected.len() == 64
            && expected.chars().all(|ch| ch.is_ascii_hexdigit())
            && file_sha256(&transaction.target)
                .is_ok_and(|actual| actual.eq_ignore_ascii_case(expected))
    });
    let verified = version_verified && hash_verified;
    if !verified {
        let failed = directory.join(format!("AskHuman.failed-{}.exe", uuid::Uuid::new_v4()));
        let _ = std::fs::rename(&transaction.target, failed);
        if let Some(backup) = backup.as_ref() {
            std::fs::rename(backup, &transaction.target)
                .context("updated binary verification failed and rollback failed")?;
        }
        return Err(anyhow!(
            "updated binary verification failed; restored previous version"
        ));
    }

    if restart_daemon {
        let _ = Command::new(&transaction.target)
            .args(["daemon", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    if restart_gui_host {
        let mut command = Command::new(&transaction.target);
        command
            .arg("--gui-host")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::daemon::spawn::configure_background(&mut command);
        let _ = command.spawn();
    }
    let _ = std::fs::remove_file(claimed_path);
    Ok(())
}

#[cfg(windows)]
fn file_sha256(path: &std::path::Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

#[cfg(not(windows))]
pub fn run_windows_worker(_args: &[String]) -> Result<()> {
    Err(anyhow!(
        "Windows update worker is unavailable on this platform"
    ))
}

/// 流式下载到文件，按内容长度回调进度。
async fn download_with_progress(
    url: &str,
    dest: &std::path::Path,
    progress: Option<ProgressCb>,
) -> Result<()> {
    use std::io::Write;
    let mut resp = http_client()
        .get(url)
        .send()
        .await
        .context("下载请求失败")?;
    if !resp.status().is_success() {
        return Err(anyhow!("下载失败：HTTP {}", resp.status()));
    }
    let total = resp.content_length();
    let mut file = std::fs::File::create(dest).context("创建下载文件失败")?;
    let mut downloaded: u64 = 0;
    while let Some(chunk) = resp.chunk().await.context("下载数据失败")? {
        file.write_all(&chunk).context("写入下载文件失败")?;
        downloaded += chunk.len() as u64;
        if let Some(cb) = &progress {
            let percentage = match total {
                Some(t) if t > 0 => (downloaded as f64 / t as f64) * 100.0,
                _ => 0.0,
            };
            cb(DownloadProgress {
                downloaded,
                total,
                percentage,
            });
        }
    }
    Ok(())
}

/// 解压 tar.gz / zip（shell out；mac/Linux 自带 tar / unzip）。
fn extract_archive(archive: &std::path::Path, dest: &std::path::Path) -> Result<()> {
    use std::process::Command;
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let out = if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        #[cfg(windows)]
        return Err(anyhow!("Windows release assets must use zip archives"));
        #[cfg(not(windows))]
        Command::new("tar")
            .args([
                "-xzf",
                &archive.to_string_lossy(),
                "-C",
                &dest.to_string_lossy(),
            ])
            .output()
            .context("执行 tar 失败")?
    } else if name.ends_with(".zip") {
        #[cfg(windows)]
        let command = "tar.exe";
        #[cfg(not(windows))]
        let command = "unzip";
        #[cfg(windows)]
        let args = vec![
            "-xf".to_string(),
            archive.to_string_lossy().to_string(),
            "-C".to_string(),
            dest.to_string_lossy().to_string(),
        ];
        #[cfg(not(windows))]
        let args = vec![
            "-o".to_string(),
            archive.to_string_lossy().to_string(),
            "-d".to_string(),
            dest.to_string_lossy().to_string(),
        ];
        Command::new(command)
            .args(args)
            .output()
            .context("执行 zip 解压工具失败")?
    } else {
        return Err(anyhow!("不支持的压缩格式：{name}"));
    };
    if !out.status.success() {
        return Err(anyhow!(
            "解压失败：{}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

/// 递归查找名为 `AskHuman` 的可执行文件。
fn find_executable(dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let entries = std::fs::read_dir(&d).ok()?;
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str())
                == Some(if cfg!(windows) {
                    "AskHuman.exe"
                } else {
                    "AskHuman"
                })
            {
                return Some(p);
            }
        }
    }
    None
}

/// macOS：校验下载二进制的签名有效性与 TeamID。
#[cfg(target_os = "macos")]
fn verify_macos_signature(path: &std::path::Path) -> Result<()> {
    use std::process::Command;
    let verify = Command::new("codesign")
        .args(["--verify", "--strict", &path.to_string_lossy()])
        .output()
        .context("执行 codesign --verify 失败")?;
    if !verify.status.success() {
        return Err(anyhow!(
            "签名校验失败：{}",
            String::from_utf8_lossy(&verify.stderr)
        ));
    }
    let info = Command::new("codesign")
        .args(["-dvv", &path.to_string_lossy()])
        .output()
        .context("执行 codesign -dvv 失败")?;
    // codesign 把信息打到 stderr。
    let text = String::from_utf8_lossy(&info.stderr);
    if !text.contains(&format!("TeamIdentifier={}", super::EXPECTED_TEAM_ID)) {
        return Err(anyhow!(
            "签名 TeamIdentifier 不符（期望 {}）",
            super::EXPECTED_TEAM_ID
        ));
    }
    Ok(())
}

/// 生成备份路径 `<exe>.<版本>.bak`（同名冲突追加序号）。
fn backup_path(current: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = current.parent()?;
    let stem = current.file_name()?.to_str()?;
    let ver = super::current_version();
    let mut p = dir.join(format!("{stem}.{ver}.bak"));
    let mut i = 1;
    while p.exists() {
        p = dir.join(format!("{stem}.{ver}.bak.{i}"));
        i += 1;
        if i > 100 {
            return None;
        }
    }
    Some(p)
}

/// 极简「作用域退出时删目录」守卫（避免引入 scopeguard crate）。
#[cfg(unix)]
fn scopeguard(dir: std::path::PathBuf) -> impl Drop {
    struct CleanupGuard(std::path::PathBuf);
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    CleanupGuard(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(windows)]
    #[tokio::test]
    async fn unsigned_windows_apply_is_blocked_before_download() {
        let error = DirectUpdater::new().apply(None).await.unwrap_err();
        assert!(error.to_string().contains("automatic update"));
    }

    #[test]
    fn asset_match_by_triple() {
        let release = json!({
            "assets": [
                { "name": "AskHuman-x86_64-unknown-linux-gnu-v0.6.0.tar.gz",
                  "browser_download_url": "https://example.com/linux.tar.gz" },
                { "name": "AskHuman-aarch64-apple-darwin-v0.6.0.tar.gz",
                  "browser_download_url": "https://example.com/mac-arm.tar.gz" },
                { "name": "AskHuman-x86_64-pc-windows-msvc-v0.6.0.zip",
                  "browser_download_url": "https://example.com/win.zip" }
            ]
        });
        assert_eq!(
            asset_url_for_triple(&release, "aarch64-apple-darwin").as_deref(),
            Some("https://example.com/mac-arm.tar.gz")
        );
        assert_eq!(
            asset_url_for_triple(&release, "x86_64-unknown-linux-gnu").as_deref(),
            Some("https://example.com/linux.tar.gz")
        );
        assert_eq!(
            asset_url_for_triple(&release, "riscv64-unknown-linux-gnu"),
            None
        );
    }

    #[test]
    fn sha256sums_lookup_matches_exact_file() {
        let sums = "abc123  AskHuman-aarch64-apple-darwin-v0.9.1.tar.gz\n\
                    def456 *AskHuman-x86_64-pc-windows-msvc-v0.9.1.zip\n";
        assert_eq!(
            expected_sha256(sums, "AskHuman-aarch64-apple-darwin-v0.9.1.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            expected_sha256(sums, "AskHuman-x86_64-pc-windows-msvc-v0.9.1.zip").as_deref(),
            Some("def456"),
            "binary-mode `*` marker must be tolerated"
        );
        assert_eq!(expected_sha256(sums, "other.tar.gz"), None);
    }

    #[test]
    fn asset_lookup_by_name_is_exact() {
        let release = json!({
            "assets": [
                { "name": "SHA256SUMS", "browser_download_url": "https://example.com/sums" },
                { "name": "AskHuman-x.tar.gz", "browser_download_url": "https://example.com/x" }
            ]
        });
        assert_eq!(
            asset_url_by_name(&release, "SHA256SUMS").as_deref(),
            Some("https://example.com/sums")
        );
        assert_eq!(asset_url_by_name(&release, "missing"), None);
    }

    #[test]
    fn asset_match_handles_missing_assets() {
        assert_eq!(
            asset_url_for_triple(&json!({}), "aarch64-apple-darwin"),
            None
        );
        assert_eq!(
            asset_url_for_triple(&json!({"assets": []}), "aarch64-apple-darwin"),
            None
        );
    }
}
