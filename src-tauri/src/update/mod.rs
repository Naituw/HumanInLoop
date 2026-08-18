//! 版本自更新：检查最新版本、按安装方式选择更新器、应用更新。
//!
//! 核心约定（见 `docs/specs/self-update.md`）：**apply 只把新二进制落盘**，
//! 「答完所有在途弹窗后再换新、不打断作答」由既有 daemon graceful-drain 完成——
//! 本模块**不调用任何进程 restart**。
//!
//! 两套实现（按 `current_exe()` 路径判定安装方式，运行时择一）：
//! - [`direct::DirectUpdater`]：GitHub Releases 查版本 + 下载平台资产原子替换；
//! - [`npm::NpmUpdater`]：npm registry 查版本 + 跑 `npm i -g askhuman@latest`。

use anyhow::Result;
use serde::{Deserialize, Serialize};

pub mod direct;
pub mod notes;
pub mod npm;
pub mod state;

#[cfg(windows)]
pub(crate) fn cleanup_stale_windows_workdirs() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(24 * 60 * 60))
        .unwrap_or(std::time::UNIX_EPOCH);
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("askhuman_update_") && !name.starts_with("askhuman_npm_update_") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < cutoff);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(windows)]
pub(crate) fn windows_worker_log_files(
    kind: &str,
) -> std::io::Result<(std::fs::File, std::fs::File)> {
    use std::io::Write;

    let directory = crate::paths::config_dir();
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!("{kind}-update-worker.log"));
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() > 1024 * 1024)
    {
        let _ = std::fs::remove_file(&path);
    }
    let mut stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(
        stdout,
        "\n--- worker started at {:?} ---",
        std::time::SystemTime::now()
    )?;
    let stderr = stdout.try_clone()?;
    Ok((stdout, stderr))
}

/// GitHub 仓库（更新检查 / 资产下载 / release notes 的来源）。
pub const GITHUB_OWNER: &str = "Naituw";
pub const GITHUB_REPO: &str = "AskHuman";
/// npm 主包名（npm 安装方式的版本检查与更新目标）。
pub const NPM_PACKAGE: &str = "askhuman";
/// 期望的 macOS 签名团队标识（替换前校验，保证完整性与钥匙串信任连续）。
pub const EXPECTED_TEAM_ID: &str = "DMJXDB9H6Q";
/// Windows release artifacts are currently unsigned. Keep automatic apply disabled until a
/// separate signing project defines and verifies the complete publisher trust chain.
pub const WINDOWS_AUTOMATIC_APPLY_ENABLED: bool = false;

/// 安装方式（决定用哪套更新器）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallKind {
    /// npm 全局安装：`current_exe` 在 `node_modules/@humaninloop|askhuman` 之下。
    Npm,
    /// 直装二进制：`install.sh` / 手动下载（如 `~/.local/bin`）。
    Direct,
}

/// How the current binary may apply an available update.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum UpdateApplyMode {
    Automatic,
    #[default]
    ManualDirect,
    ManualNpm,
}

/// 一次检查的对外结果（供 daemon 落盘、前端展示）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// 远端正式版是否高于本地。
    pub available: bool,
    pub current_version: String,
    pub latest_version: String,
    /// 最新版更新日志（markdown），可空。
    pub release_notes: String,
    /// Platform asset or release page URL. This is always a URL, never a shell command.
    pub source_url: String,
    /// 安装方式是否为 npm（前端据此决定按钮文案 / 更新行为）。
    pub is_npm: bool,
    /// Automatic apply on macOS/Linux; manual Direct/npm paths on unsigned Windows.
    #[serde(default)]
    pub apply_mode: UpdateApplyMode,
    /// Fixed manual npm command. Empty for Direct installations.
    #[serde(default)]
    pub manual_command: String,
}

/// 更新器内部的「远端最新版」查询结果。
#[derive(Debug, Clone)]
pub struct RemoteLatest {
    /// 规范化版本号（去掉前缀 `v` 与非数字字符）。
    pub version: String,
    /// 最新版更新日志（markdown），可空。
    pub notes: String,
    /// 直装资产 URL / release 页（兜底）/ npm 命令提示。
    pub source_url: String,
}

/// 下载进度（apply 期间回调给调用方，用于前端进度条）。
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percentage: f64,
}

/// 下载进度回调（由调用方提供，如向 webview emit 事件）。
pub type ProgressCb = Box<dyn Fn(DownloadProgress) + Send + Sync>;

/// 统一更新器抽象：查最新版 + 应用更新（落盘，不 restart）。
#[async_trait::async_trait]
pub trait Updater: Send + Sync {
    /// 查询远端最新正式版（不做版本比较、不落盘）。
    async fn check_latest(&self, fresh: bool) -> Result<RemoteLatest>;
    /// 应用更新：把新二进制写到盘上（DirectUpdater 替换文件 / NpmUpdater 跑 npm）。
    /// **不** restart；换新交给 daemon drain。
    async fn apply(&self, progress: Option<ProgressCb>) -> Result<()>;
}

/// 本地当前版本（编译期嵌入）。
pub fn current_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// 当前平台对应的 rust 目标三元组（用于匹配 Release 资产名）。非预期平台返回 None。
pub fn target_triple() -> Option<&'static str> {
    Some(match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    })
}

/// 当前安装方式（读 `current_exe()` 路径）。
pub fn detect_install_kind() -> InstallKind {
    match std::env::current_exe() {
        Ok(p) => install_kind_from_path(&p.to_string_lossy()),
        Err(_) => InstallKind::Direct,
    }
}

pub fn apply_mode_for(platform: &str, kind: InstallKind) -> UpdateApplyMode {
    if platform != "windows" || WINDOWS_AUTOMATIC_APPLY_ENABLED {
        return UpdateApplyMode::Automatic;
    }
    match kind {
        InstallKind::Direct => UpdateApplyMode::ManualDirect,
        InstallKind::Npm => UpdateApplyMode::ManualNpm,
    }
}

pub fn apply_mode() -> UpdateApplyMode {
    apply_mode_for(std::env::consts::OS, detect_install_kind())
}

pub fn ensure_automatic_apply_allowed() -> Result<()> {
    if apply_mode() == UpdateApplyMode::Automatic {
        Ok(())
    } else {
        anyhow::bail!(
            "Windows automatic update is currently unavailable; prepare and install the update manually"
        )
    }
}

pub fn release_url(version: &str) -> String {
    format!("https://github.com/{GITHUB_OWNER}/{GITHUB_REPO}/releases/tag/v{version}")
}

#[cfg(windows)]
pub fn spawn_manual_prepare() -> Result<()> {
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};

    if apply_mode() == UpdateApplyMode::Automatic {
        anyhow::bail!("manual update preparation is unavailable for this build");
    }
    let executable = std::env::current_exe()?;
    let (stdout, stderr) = windows_worker_log_files("manual-update-prepare")?;
    Command::new(executable)
        .args(["update", "prepare"])
        .creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()?;
    Ok(())
}

#[cfg(not(windows))]
pub fn spawn_manual_prepare() -> Result<()> {
    anyhow::bail!("manual update preparation is only available on Windows")
}

/// 纯函数：由路径字符串判定安装方式（便于单测）。
pub fn install_kind_from_path(path: &str) -> InstallKind {
    let s = path.replace('\\', "/");
    if s.contains("/node_modules/@humaninloop/") || s.contains("/node_modules/askhuman/") {
        InstallKind::Npm
    } else {
        InstallKind::Direct
    }
}

/// 按安装方式选择更新器。
pub fn select_updater() -> Box<dyn Updater> {
    match detect_install_kind() {
        InstallKind::Npm => Box::new(npm::NpmUpdater::new()),
        InstallKind::Direct => Box::new(direct::DirectUpdater::new()),
    }
}

/// 完整检查：查远端最新版 + 与本地比较，返回对外结果。
pub async fn check() -> Result<UpdateInfo> {
    check_with_freshness(false).await
}

/// Manual checks ask upstream caches to revalidate so a just-published release is not hidden by a
/// previously cached `/latest` response.
pub async fn check_fresh() -> Result<UpdateInfo> {
    check_with_freshness(true).await
}

async fn check_with_freshness(fresh: bool) -> Result<UpdateInfo> {
    let kind = detect_install_kind();
    let updater = select_updater();
    let latest = updater.check_latest(fresh).await?;
    let current = current_version();
    let available = compare_versions(&latest.version, &current) > 0;
    let apply_mode = apply_mode_for(std::env::consts::OS, kind);
    let source_url = if apply_mode == UpdateApplyMode::ManualDirect {
        release_url(&latest.version)
    } else {
        latest.source_url
    };
    Ok(UpdateInfo {
        available,
        current_version: current,
        latest_version: latest.version,
        release_notes: latest.notes,
        source_url,
        is_npm: matches!(kind, InstallKind::Npm),
        apply_mode,
        manual_command: matches!(kind, InstallKind::Npm)
            .then(npm::NpmUpdater::manual_command)
            .unwrap_or_default(),
    })
}

/// Persist a successful check and reconcile it with the highest version observed by any process.
/// The returned value is what callers should display and broadcast.
pub fn persist_check_result(mut info: UpdateInfo, clear_dismissed: bool) -> UpdateInfo {
    let stored = state::record_check(&info.latest_version, &info.release_notes, clear_dismissed);
    if stored.latest_version != info.latest_version {
        info.latest_version = stored.latest_version;
        info.release_notes = stored.release_notes;
    }
    info.available = compare_versions(&info.latest_version, &info.current_version) > 0;
    if info.apply_mode != UpdateApplyMode::Automatic {
        info.source_url = release_url(&info.latest_version);
    }
    info
}

/// 规范化版本号：去前缀 `v`、丢弃预发布后缀、仅保留数字与点。
pub fn normalize_version(tag: &str) -> String {
    let core = tag.trim().trim_start_matches('v');
    // 丢弃 `-rc.1` 等预发布后缀（一期自更新只面向正式版）。
    let core = core.split('-').next().unwrap_or(core);
    core.chars()
        .filter(|c| c.is_ascii_digit() || *c == '.')
        .collect()
}

/// 逐段数字比较：a>b → 1，a<b → -1，相等 → 0。非数字段按 0 处理。
pub fn compare_versions(a: &str, b: &str) -> i32 {
    let pa: Vec<u64> = a.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    let pb: Vec<u64> = b.split('.').map(|s| s.parse().unwrap_or(0)).collect();
    let n = pa.len().max(pb.len());
    for i in 0..n {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x > y {
            return 1;
        }
        if x < y {
            return -1;
        }
    }
    0
}

/// 共享 HTTP 客户端（带 UA 与超时；GitHub API 要求 UA）。
/// 仅用于**非 GitHub-API**请求（npm registry / release 资产下载）——不带任何鉴权头，
/// 避免把 GitHub token 泄露给 npmjs / 资产 CDN。
pub(crate) fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("AskHuman-self-update")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 读取可选 GitHub token（`ASKHUMAN_GITHUB_TOKEN` 优先，回退 `GITHUB_TOKEN`）。
/// 设置后未认证额度 60/时/IP → 认证 5000/时/账号，根治共享出口 IP（如代理）被占满的 403。
pub(crate) fn github_token() -> Option<String> {
    for key in ["ASKHUMAN_GITHUB_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    None
}

/// GitHub **API** 专用客户端：UA + 超时 +（若有 token）`Authorization: Bearer`。
/// token 头标记为 sensitive，不进调试日志。
pub(crate) fn github_client() -> reqwest::Client {
    use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
    let mut headers = HeaderMap::new();
    if let Some(token) = github_token() {
        if let Ok(mut val) = HeaderValue::from_str(&format!("Bearer {token}")) {
            val.set_sensitive(true);
            headers.insert(AUTHORIZATION, val);
        }
    }
    reqwest::Client::builder()
        .user_agent("AskHuman-self-update")
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// 把 GitHub API 的失败状态码归类为可识别错误：403/429（限流）统一带 `rate-limited` 标记，
/// 供前端映射为友好文案（共享 IP 额度用尽、引导手动下载 / 设 token）。
pub(crate) fn github_status_error(status: reqwest::StatusCode) -> anyhow::Error {
    if matches!(status.as_u16(), 403 | 429) {
        anyhow::anyhow!("rate-limited (HTTP {})", status)
    } else {
        anyhow::anyhow!("GitHub API 返回 {}", status)
    }
}

/// 拼 GitHub API URL（`repos/{owner}/{repo}/{path}`）。
pub(crate) fn github_api_url(path: &str) -> String {
    format!(
        "https://api.github.com/repos/{}/{}/{}",
        GITHUB_OWNER, GITHUB_REPO, path
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compare_versions_basic() {
        assert_eq!(compare_versions("0.5.4", "0.5.3"), 1);
        assert_eq!(compare_versions("0.5.3", "0.5.3"), 0);
        assert_eq!(compare_versions("0.5.3", "0.5.4"), -1);
        assert_eq!(compare_versions("1.0.0", "0.9.9"), 1);
        // 段数不同：缺省段按 0。
        assert_eq!(compare_versions("0.5", "0.5.0"), 0);
        assert_eq!(compare_versions("0.5.1", "0.5"), 1);
        assert_eq!(compare_versions("0.6.0", "0.5.99"), 1);
    }

    #[test]
    fn normalize_version_strips_prefix_and_prerelease() {
        assert_eq!(normalize_version("v0.5.3"), "0.5.3");
        assert_eq!(normalize_version("0.5.3"), "0.5.3");
        assert_eq!(normalize_version("v0.6.0-rc.1"), "0.6.0");
        assert_eq!(normalize_version(" v1.2.3 "), "1.2.3");
    }

    #[test]
    fn install_kind_detects_npm_paths() {
        assert_eq!(
            install_kind_from_path(
                "/Users/x/.../node_modules/@humaninloop/darwin-arm64/bin/AskHuman"
            ),
            InstallKind::Npm
        );
        assert_eq!(
            install_kind_from_path("/usr/lib/node_modules/askhuman/bin/cli.js"),
            InstallKind::Npm
        );
        assert_eq!(
            install_kind_from_path("/Users/x/.local/bin/AskHuman"),
            InstallKind::Direct
        );
        // Windows 风格反斜杠路径也应识别。
        assert_eq!(
            install_kind_from_path(
                "C:\\Users\\x\\node_modules\\@humaninloop\\win32-x64\\bin\\AskHuman.exe"
            ),
            InstallKind::Npm
        );
    }

    #[test]
    fn unsigned_windows_uses_manual_apply_modes() {
        assert_eq!(
            apply_mode_for("windows", InstallKind::Direct),
            UpdateApplyMode::ManualDirect
        );
        assert_eq!(
            apply_mode_for("windows", InstallKind::Npm),
            UpdateApplyMode::ManualNpm
        );
    }

    #[cfg(windows)]
    #[test]
    fn unsigned_windows_rejects_automatic_apply() {
        assert!(ensure_automatic_apply_allowed().is_err());
    }

    #[test]
    fn unix_platforms_keep_automatic_apply() {
        for platform in ["macos", "linux"] {
            assert_eq!(
                apply_mode_for(platform, InstallKind::Direct),
                UpdateApplyMode::Automatic
            );
            assert_eq!(
                apply_mode_for(platform, InstallKind::Npm),
                UpdateApplyMode::Automatic
            );
        }
    }

    #[test]
    fn missing_apply_mode_deserializes_fail_closed() {
        let info: UpdateInfo = serde_json::from_value(serde_json::json!({
            "available": true,
            "currentVersion": "1.1.0",
            "latestVersion": "1.2.0",
            "releaseNotes": "",
            "sourceUrl": "https://example.invalid/release",
            "isNpm": false
        }))
        .unwrap();
        assert_eq!(info.apply_mode, UpdateApplyMode::ManualDirect);
        assert!(info.manual_command.is_empty());
    }

    #[test]
    fn target_triple_present_on_supported_platforms() {
        // 至少在三个一期目标上有值（CI/本地都覆盖其一）。
        let t = target_triple();
        if cfg!(any(
            all(target_os = "macos", target_arch = "aarch64"),
            all(target_os = "macos", target_arch = "x86_64"),
            all(target_os = "linux", target_arch = "x86_64"),
            all(target_os = "windows", target_arch = "x86_64"),
        )) {
            assert!(t.is_some());
        }
    }
}
