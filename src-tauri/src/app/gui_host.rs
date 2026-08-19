//! 统一 GUI 宿主进程的运行时（spec D2/D5/D10/D11）：单实例托盘图标 + 设置/历史/Agent 窗口。
//!
//! 由 `app::run_gui_host` 经 `app::launch(View::GuiHost)` 进入；本模块负责：
//! - 单实例 flock（`gui-host.lock`）+ macOS 活动策略（有图标时 accessory）。
//! - 自有 IPC 监听（`gui-host.sock`）：收 `OpenWindow` → 主线程聚焦/新建唯一窗口。
//! - daemon 客户端：一条**非保活**的 `TraySubscribe`（驱动图标/菜单），外加「有窗口时」一条
//!   普通连接给 daemon 续命（spec D5）。
//! - 配置监听：菜单栏模式 / 语言变化 → 重建菜单 + 装/卸登录项 + 切活动策略。
//! - 二进制换新：盘上二进制变化且无窗口时 re-exec / 交 launchd（spec D11）。

use crate::app::tray_menu::{Node, TrayMenu};
use crate::config::{AppConfig, DaemonLifecycleMode, MenuBarIconMode, ThemeMode};
use crate::daemon::lifecycle::{self, Fingerprint, LockGuard};
use crate::gui_host::{HostMsg, WindowKind};
use crate::i18n::{self, Lang};
use crate::ipc::{self, transport, ClientMsg, ServerMsg};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tauri::image::Image;
use tauri::menu::Menu;
use tauri::tray::{TrayIcon, TrayIconBuilder};
use tauri::{AppHandle, Manager};
use tokio::io::BufReader;
use tokio::sync::Notify;

const QUICK_ASK_INTERJECT: &str =
    "I have a new thought. Before continuing, ask me immediately using the questioning tool described in your instructions.";

/// 某 label 是否为宿主统一承载的窗口（用于窗口计数 / 续命判定）。
/// 插话 composer 窗口每 session 一个，label 动态（`interject-<hash>`），按前缀识别。
pub fn is_hosted_label(label: &str) -> bool {
    matches!(
        label,
        "settings" | "history" | "agents" | "todos" | "newtask"
    ) || label.starts_with("interject-")
}

// ===== 内嵌图标资源（三态；统一单色模板图）=====
//
// 三态图标由 `scripts/gen_tray_icons.py` 从设计稿 `icons/tray/source.png`
// 抠图生成：黑色像素=不透明，白色（如 "?"、月牙缺口）=透明。macOS 作为
// 模板图（系统按菜单栏明暗自动反色）；Linux 直接使用同一张单色图。
mod icon_bytes {
    pub const IDLE: &[u8] = include_bytes!("../../icons/tray/tray-idle.png");
    pub const ACTIVE: &[u8] = include_bytes!("../../icons/tray/tray-active.png");
    pub const STOPPED: &[u8] = include_bytes!("../../icons/tray/tray-stopped.png");
    pub const IDLE_ATTENTION: &[u8] = include_bytes!("../../icons/tray/tray-idle-attention.png");
    pub const STOPPED_ATTENTION: &[u8] =
        include_bytes!("../../icons/tray/tray-stopped-attention.png");
    /// 仅 macOS 把单色图当作模板图染色；其它平台原样显示。
    pub const TEMPLATE: bool = cfg!(target_os = "macos");
}

// ===== 单实例锁（进程级；re-exec 时显式释放）=====

static HOST_LOCK: OnceLock<Mutex<Option<LockGuard>>> = OnceLock::new();

fn host_lock_slot() -> &'static Mutex<Option<LockGuard>> {
    HOST_LOCK.get_or_init(|| Mutex::new(None))
}

/// 获取宿主单实例锁；成功返回 true（并持有锁至进程退出 / 显式释放）。
pub fn acquire_singleton() -> bool {
    match lifecycle::acquire_lock_at(&crate::paths::gui_host_lock()) {
        Ok(Some(g)) => {
            *host_lock_slot().lock().unwrap() = Some(g);
            true
        }
        _ => false,
    }
}

/// 释放单实例锁（仅二进制换新 re-exec 前调用，让新实例可抢锁）。
fn release_lock() {
    if let Ok(mut s) = host_lock_slot().lock() {
        *s = None;
    }
}

// ===== 宿主运行态（Tauri managed）=====

#[derive(Clone, Default)]
pub struct TrayData {
    pub running: bool,
    pub version: String,
    pub uptime_secs: u64,
    pub active_requests: usize,
    pub im_connections: Vec<String>,
    pub draining: bool,
    pub agents_working: usize,
    pub agents_idle: usize,
    pub update_available: bool,
    pub update_latest: String,
    pub pending: bool,
    /// 在途请求摘要（托盘「待答」子菜单逐条列出）。
    pub pending_requests: Vec<ipc::PendingRequestInfo>,
    /// 活动 agent 摘要（托盘「Agent 状态」子菜单逐条列出，spec agent-interject D7）。
    pub agents: Vec<ipc::TrayAgentInfo>,
    /// 各渠道最近未恢复的故障（R7：托盘警示项，点击打开设置渠道 tab）。
    pub channel_issues: Vec<ipc::ChannelIssueInfo>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum UpdateActionState {
    #[default]
    Idle,
    Checking,
    Current,
    Found(String),
    CheckFailed(String),
    Applying,
    ApplyFailed(String),
    RestartFailed(String),
}

impl UpdateActionState {
    fn busy(&self) -> bool {
        matches!(self, Self::Checking | Self::Applying)
    }
}

fn cached_update_available(state: &crate::update::state::UpdateState, current: &str) -> bool {
    !state.latest_version.is_empty()
        && crate::update::compare_versions(&state.latest_version, current) > 0
        && !state
            .dismissed_versions
            .iter()
            .any(|version| version == &state.latest_version)
}

fn merge_update_status(
    daemon_available: bool,
    daemon_latest: String,
    daemon_pending: bool,
    persisted: &crate::update::state::UpdateState,
    current: &str,
) -> (bool, String, bool) {
    let persisted_wins = !persisted.latest_version.is_empty()
        && (daemon_latest.is_empty()
            || crate::update::compare_versions(&persisted.latest_version, &daemon_latest) >= 0);
    let (mut available, latest) = if persisted_wins {
        (
            cached_update_available(persisted, current),
            persisted.latest_version.clone(),
        )
    } else {
        (
            daemon_available && crate::update::compare_versions(&daemon_latest, current) > 0,
            daemon_latest,
        )
    };
    let pending = daemon_pending || persisted.pending;
    if pending {
        available = false;
    }
    (available, latest, pending)
}

/// Seed the daemon-independent part of the tray from the persisted update check.
fn initial_tray_data() -> TrayData {
    let update = crate::update::state::load();
    let (update_available, update_latest, pending) = merge_update_status(
        false,
        String::new(),
        false,
        &update,
        &crate::update::current_version(),
    );
    TrayData {
        update_available,
        update_latest,
        pending,
        ..TrayData::default()
    }
}

fn apply_checked_update(data: &mut TrayData, info: &crate::update::UpdateInfo) {
    data.update_latest = info.latest_version.clone();
    data.update_available = info.available && !data.pending;
}

/// Immediately synchronize a Settings or tray check into the long-lived GUI Host. Disk state is
/// still authoritative; this closes the gap while the daemon is stopped or before its next frame.
pub(crate) fn sync_checked_update(
    app: &AppHandle,
    info: &crate::update::UpdateInfo,
    show_feedback: bool,
) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    apply_checked_update(&mut state.data.lock().unwrap(), info);
    if show_feedback {
        *state.update_action.lock().unwrap() = if info.available {
            UpdateActionState::Found(info.latest_version.clone())
        } else {
            UpdateActionState::Current
        };
    }
    refresh_on_main(app);
}

pub(crate) fn sync_update_check_error(app: &AppHandle, error: &str) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    *state.update_action.lock().unwrap() =
        UpdateActionState::CheckFailed(compact_update_error(error));
    refresh_on_main(app);
}

pub struct HostState {
    pub mode: Mutex<MenuBarIconMode>,
    /// 上次已应用的守护进程生命周期模式：用于在配置变更时检测「切到保活」的跃迁，
    /// 从而立即拉起 daemon（打开开关即视为一次触发，见 spec）。
    pub daemon_lifecycle: Mutex<DaemonLifecycleMode>,
    pub lang: Mutex<Lang>,
    pub data: Mutex<TrayData>,
    /// Current-mode integration bundles that need reconciliation, in settings display order.
    pub integration_updates: Mutex<Vec<String>>,
    /// GUI Host owns menu-triggered update feedback even while daemon is down.
    update_action: Mutex<UpdateActionState>,
    pub daemon_up: AtomicBool,
    pub windows_open: AtomicUsize,
    /// 是否曾经打开过窗口（off/active 模式退出判定用，避免开窗前误退）。
    pub ever_open: AtomicBool,
    /// 启动宽限是否已过（覆盖「OpenWindow 始终未到达」的兜底退出）。
    pub grace_over: AtomicBool,
    pub tray: Mutex<Option<TrayIcon>>,
    /// 持久托盘菜单（**长期持有同一个** `Menu` 对象 + 影子树）：刷新时按 `key` 做 diff，只就地改条目
    /// 文字 / 可用性，结构变化才最小增删——绝不整段重建或 `set_menu` 换对象（spec 菜单稳定性）。
    pub tray_menu: Mutex<Option<TrayMenu>>,
    /// 上次渲染的「菜单/图标内容签名」：与本次相同则整次刷新直接跳过（连 diff 都不做）——
    /// daemon 持续停止时内容不变 → 不触碰菜单 → 展开的菜单不会被挤掉。
    pub menu_sig: Mutex<Option<String>>,
    /// 窗口期续命连接的停止信号（持有即有续命连接在）。
    pub keepalive: Mutex<Option<Arc<Notify>>>,
    /// Agent 状态订阅的停止信号（与 agent 窗口绑定）：开窗（前端就绪）时重启、关窗时停。
    /// 长命宿主下若复用旧订阅，daemon 不会重推首帧 → 窗口首屏长时间 Loading，故每次开窗都重启。
    pub agents_sub: Mutex<Option<Arc<Notify>>>,
    /// 启动时的二进制指纹（盘上内容变化即触发宿主换新）。
    pub startup_fp: Fingerprint,
    /// Stable on-disk launch path captured before self-update replaces the running inode.
    pub executable_path: PathBuf,
    /// 盘上二进制已与启动时不同（由 15s 轮询维护）：有窗口挡住自动换新时，
    /// 托盘显示「重启以完成更新」项（B2）；也作为关最后一个窗口即刻换新的提示（B1）。
    pub binary_stale: AtomicBool,
}

impl HostState {
    fn mode(&self) -> MenuBarIconMode {
        *self.mode.lock().unwrap()
    }
    fn lang(&self) -> Lang {
        *self.lang.lock().unwrap()
    }
}

/// 当前平台是否支持托盘（macOS 恒真；Linux 需图形会话）。
fn tray_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
    #[cfg(windows)]
    {
        true
    }
}

// ===== 入口：在 launch() 的 setup 中调用 =====

/// 在 Tauri setup（主线程）中初始化宿主：活动策略 + 托盘 + IPC + daemon 客户端 + 配置监听。
pub fn setup(app: &mut tauri::App, config: &AppConfig) -> tauri::Result<()> {
    let lang = Lang::resolve(&config.general.language);
    let mode = config.general.menu_bar_icon;
    let executable_path = std::env::current_exe().unwrap_or_default();
    let startup_fp = if executable_path.as_os_str().is_empty() {
        lifecycle::current_fingerprint()
    } else {
        lifecycle::fingerprint_at(&executable_path)
    };

    app.manage(HostState {
        mode: Mutex::new(mode),
        daemon_lifecycle: Mutex::new(config.general.daemon_lifecycle),
        lang: Mutex::new(lang),
        data: Mutex::new(initial_tray_data()),
        integration_updates: Mutex::new(detect_integration_updates()),
        update_action: Mutex::new(UpdateActionState::Idle),
        daemon_up: AtomicBool::new(false),
        windows_open: AtomicUsize::new(0),
        ever_open: AtomicBool::new(false),
        grace_over: AtomicBool::new(false),
        tray: Mutex::new(None),
        tray_menu: Mutex::new(None),
        menu_sig: Mutex::new(None),
        keepalive: Mutex::new(None),
        agents_sub: Mutex::new(None),
        startup_fp,
        executable_path,
        binary_stale: AtomicBool::new(false),
    });

    // 初始活动策略（macOS）：有图标 → accessory（不占 Dock/Cmd-Tab）；off → regular（窗口正常入坞）。
    #[cfg(target_os = "macos")]
    {
        let policy = if mode == MenuBarIconMode::Off {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        app.set_activation_policy(policy);

        // tao launched() 在 setup hook 之前以 .regular + activateIgnoringOtherApps(true)
        // 激活了应用。对无窗口的状态栏 app，这导致 macOS 在首次展开子菜单时隐式关闭菜单。
        // 详见 docs/investigations/tray-menu-close-on-first-hover.md
        if mode != MenuBarIconMode::Off {
            let mtm = objc2_foundation::MainThreadMarker::new().unwrap();
            let ns_app = objc2_app_kit::NSApp(mtm);
            ns_app.deactivate();
            #[allow(deprecated)]
            ns_app.activateIgnoringOtherApps(false);
        }
    }

    // 有图标且托盘可用 → 建托盘（初始 idle 图标，随后由状态订阅刷新）。
    if mode != MenuBarIconMode::Off && tray_supported() {
        let _ = ensure_tray(&app.handle().clone(), true);
    }
    // always 模式装登录项（best-effort）。
    if mode == MenuBarIconMode::Always {
        let _ = crate::integrations::login_item::ensure_installed();
    }

    let handle = app.handle().clone();
    start_ipc_listener(handle.clone());
    start_status_subscription(handle.clone());
    start_config_watch(handle.clone());
    start_binary_watch(handle.clone());

    // 启动宽限：12s 后标记并复核退出（覆盖 OpenWindow 始终未到达的极端情况）。
    let h = handle.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(12)).await;
        if let Some(state) = h.try_state::<HostState>() {
            state.grace_over.store(true, Ordering::SeqCst);
        }
        evaluate_exit(&h);
    });

    Ok(())
}

/// macOS 活动策略：有图标（菜单栏 app）→ accessory（不占 Dock/Cmd-Tab）；off → regular（窗口正常入坞）。
fn apply_activation_policy(app: &AppHandle, mode: MenuBarIconMode) {
    #[cfg(target_os = "macos")]
    {
        let policy = if mode == MenuBarIconMode::Off {
            tauri::ActivationPolicy::Regular
        } else {
            tauri::ActivationPolicy::Accessory
        };
        let _ = app.set_activation_policy(policy);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, mode);
    }
}

// ===== 托盘图标 / 菜单 =====

fn decode_icon(bytes: &'static [u8]) -> Option<Image<'static>> {
    let image = Image::from_bytes(bytes).ok()?;
    #[cfg(target_os = "windows")]
    {
        Some(compact_windows_tray_icon(image))
    }
    #[cfg(not(target_os = "windows"))]
    {
        Some(image)
    }
}

/// Trim transparent pixels and place the result on a square canvas with a one-pixel safety edge.
/// Windows scales the whole source canvas into the notification area, so the macOS-oriented 4:3
/// artwork otherwise appears visibly smaller than neighboring tray icons.
#[cfg(any(target_os = "windows", test))]
fn compact_windows_tray_icon(image: Image<'static>) -> Image<'static> {
    const PADDING: u32 = 1;

    let width = image.width();
    let height = image.height();
    let rgba = image.rgba();
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;

    for y in 0..height {
        for x in 0..width {
            let alpha = rgba[((y * width + x) * 4 + 3) as usize];
            if alpha == 0 {
                continue;
            }
            found = true;
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }

    if !found {
        return image;
    }

    let content_width = max_x - min_x + 1;
    let content_height = max_y - min_y + 1;
    let side = content_width.max(content_height) + PADDING * 2;
    let target_x = (side - content_width) / 2;
    let target_y = (side - content_height) / 2;
    let mut compact = vec![0; (side * side * 4) as usize];

    for row in 0..content_height {
        let source_start = (((min_y + row) * width + min_x) * 4) as usize;
        let source_end = source_start + (content_width * 4) as usize;
        let target_start = (((target_y + row) * side + target_x) * 4) as usize;
        compact[target_start..target_start + (content_width * 4) as usize]
            .copy_from_slice(&rgba[source_start..source_end]);
    }

    Image::new_owned(compact, side, side)
}

fn icon_source(
    daemon_up: bool,
    active_requests: usize,
    integration_attention: bool,
) -> &'static [u8] {
    match (daemon_up, active_requests > 0, integration_attention) {
        (false, _, true) => icon_bytes::STOPPED_ATTENTION,
        (false, _, false) => icon_bytes::STOPPED,
        // Pending replies take priority over the lower-severity integration reminder.
        (true, true, _) => icon_bytes::ACTIVE,
        (true, false, true) => icon_bytes::IDLE_ATTENTION,
        (true, false, false) => icon_bytes::IDLE,
    }
}

fn icon_for(
    daemon_up: bool,
    active_requests: usize,
    integration_attention: bool,
) -> Option<Image<'static>> {
    decode_icon(icon_source(
        daemon_up,
        active_requests,
        integration_attention,
    ))
}

/// 建立（present=true）或移除（present=false）托盘图标。须在主线程调用。
fn ensure_tray(app: &AppHandle, present: bool) -> tauri::Result<()> {
    let Some(state) = app.try_state::<HostState>() else {
        return Ok(());
    };
    if !present {
        *state.tray.lock().unwrap() = None; // Drop → 移除托盘
        *state.tray_menu.lock().unwrap() = None; // 菜单随托盘失效；下次重建时再新建并填充
        *state.menu_sig.lock().unwrap() = None;
        return Ok(());
    }
    if state.tray.lock().unwrap().is_some() {
        refresh_tray(app);
        return Ok(());
    }
    // 建一个空菜单挂上托盘并长期持有；条目交给 refresh_tray 经 diff 填充（之后只就地改 / 最小增删）。
    let menu = Menu::new(app)?;
    let mut builder = TrayIconBuilder::with_id("askhuman-tray")
        .icon_as_template(icon_bytes::TEMPLATE)
        .menu(&menu)
        .show_menu_on_left_click(true);
    if let Some(img) = decode_icon(icon_bytes::IDLE) {
        builder = builder.icon(img);
    }
    let tray = builder.build(app)?;
    *state.tray.lock().unwrap() = Some(tray);
    *state.tray_menu.lock().unwrap() = Some(TrayMenu::new(app.clone(), menu));
    // 强制首刷：清掉签名缓存，确保 refresh_tray 一定填充菜单 + 摆正图标/tooltip。
    *state.menu_sig.lock().unwrap() = None;
    refresh_tray(app);
    Ok(())
}

/// 按最近状态刷新图标 + 菜单 + tooltip。须在主线程调用。
///
/// 关键：**内容没变就整次跳过**（连菜单条目都不碰）；有变化时也只**原地增删菜单条目**，
/// 绝不 `tray.set_menu` 换新对象。这样 daemon 持续停止（内容不变）时展开的菜单不会被 2s
/// 刷新挤掉；即便有变化，原地改条目通常也不会关闭已展开菜单（只有替换 NSMenu 对象才会）。
pub fn refresh_tray(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let tray_guard = state.tray.lock().unwrap();
    let Some(tray) = tray_guard.as_ref() else {
        return;
    };
    let up = state.daemon_up.load(Ordering::SeqCst);
    let data = state.data.lock().unwrap().clone();
    let integration_updates = state.integration_updates.lock().unwrap().clone();
    let update_action = state.update_action.lock().unwrap().clone();
    let lang = state.lang();
    // 是否有任一家开启了生命周期追踪：未开启时隐藏 Agent 状态相关菜单项（入口 + 忙闲行）。
    let lifecycle_on = crate::integrations::agent_lifecycle::any_installed();
    // 盘上二进制已换新但被打开的窗口挡住自动 re-exec → 菜单出现「重启以完成更新」项（B2）。
    let stale = state.binary_stale.load(Ordering::SeqCst);

    // 内容签名：与上次相同 → 整次跳过，不触碰托盘（连 diff 都省，确保展开的菜单纹丝不动）。
    let sig = menu_signature(
        up,
        lang,
        &data,
        &integration_updates,
        lifecycle_on,
        stale,
        &update_action,
    );
    if state.menu_sig.lock().unwrap().as_deref() == Some(sig.as_str()) {
        return;
    }

    // 图标（set_icon / set_tooltip 不会关闭已展开菜单）。
    if let Some(img) = icon_for(up, data.active_requests, !integration_updates.is_empty()) {
        let _ = tray.set_icon(Some(img));
        let _ = tray.set_icon_as_template(icon_bytes::TEMPLATE);
    }
    // 菜单：把期望节点列表 diff 应用到**同一个**菜单对象——文字变化只 set_text、结构变化才最小增删，
    // 绝不整段重建（整段重建会关掉已展开菜单）。
    if let Some(tm) = state.tray_menu.lock().unwrap().as_mut() {
        tm.apply(build_specs(
            up,
            lang,
            &data,
            &integration_updates,
            lifecycle_on,
            stale,
            &update_action,
        ));
    }
    let mut tip = if up {
        i18n::tr(lang, "tray.tooltipRunning").to_string()
    } else {
        i18n::tr(lang, "tray.tooltipStopped").to_string()
    };
    if !integration_updates.is_empty() {
        tip.push_str(" · ");
        tip.push_str(i18n::tr(lang, "tray.integrationUpdatesTooltip"));
    }
    let _ = tray.set_tooltip(Some(&tip));

    *state.menu_sig.lock().unwrap() = Some(sig);
}

/// 决定菜单/图标渲染结果的全部输入拼成的签名：与上次相同即「整次跳过」（菜单已是正确状态，连 diff 都省，
/// 确保展开的菜单纹丝不动）；不同才进入 diff。**必须覆盖 `build_specs` 与图标/tooltip 的每个输入**，
/// 否则真变化会被误跳过。uptime 取分钟级文案，避免秒级微变把每次推送都判为「有变化」。
fn menu_signature(
    up: bool,
    lang: Lang,
    data: &TrayData,
    integration_updates: &[String],
    lifecycle_on: bool,
    stale: bool,
    update_action: &UpdateActionState,
) -> String {
    // 待答子菜单内容（id+预览）也入签名：列表/预览变化即触发 diff。
    let pending: String = data
        .pending_requests
        .iter()
        .map(|p| format!("{}={}", p.id, p.preview))
        .collect::<Vec<_>>()
        .join(";");
    // 渠道故障（R7）也入签名：渠道 / 文案 / 分钟级时间变化即触发 diff。
    let issues: String = data
        .channel_issues
        .iter()
        .map(|i| format!("{}={}@{}", i.channel, i.message, fmt_ago(i.at_ms, lang)))
        .collect::<Vec<_>>()
        .join(";");
    // Include Agent identity, lineage, state, and action capabilities so every visible menu change
    // triggers a diff refresh.
    let agents: String = data
        .agents
        .iter()
        .map(|a| {
            format!(
                "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}",
                a.session_id,
                a.seq,
                a.kind,
                a.title,
                a.forked_from_seq.unwrap_or_default(),
                a.project_name,
                a.state,
                a.pending_interject as u8,
                a.focusable as u8,
                a.forkable as u8
            )
        })
        .collect::<Vec<_>>()
        .join(";");
    format!(
        "{:?}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{}|{:?}",
        lang,
        up as u8,
        data.version,
        fmt_uptime(data.uptime_secs),
        data.draining as u8,
        data.active_requests,
        data.agents_working,
        data.agents_idle,
        data.im_connections.join(","),
        data.update_available as u8,
        // Available / pending rows both name the target version. Ignore the cached remote value
        // only when neither row is visible, avoiding irrelevant menu refreshes.
        if data.update_available || data.pending {
            data.update_latest.as_str()
        } else {
            ""
        },
        lifecycle_on as u8,
        data.pending as u8,
        stale as u8,
        pending,
        agents,
        issues,
        integration_updates.join(","),
        update_action,
    )
}

fn detect_integration_updates() -> Vec<String> {
    use crate::integrations::agent_rules::AgentTarget;

    [
        ("cursor", AgentTarget::Cursor),
        ("claude", AgentTarget::ClaudeCode),
        ("codex", AgentTarget::Codex),
        ("grok", AgentTarget::Grok),
        ("pi", AgentTarget::Pi),
    ]
    .into_iter()
    .filter(|&(_, target)| crate::integrations::agent_mode::needs_update(target))
    .map(|(id, _)| id.to_string())
    .collect()
}

/// Recheck host-local Agent integration artifacts and redraw the tray only when the result changes.
/// Called on Host startup, each daemon down-to-up transition, and successful Settings mutations.
pub(crate) fn refresh_integration_updates(app: &AppHandle) {
    let updates = detect_integration_updates();
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let changed = {
        let mut current = state.integration_updates.lock().unwrap();
        if *current == updates {
            false
        } else {
            *current = updates;
            true
        }
    };
    if changed {
        refresh_on_main(app);
    }
}

fn integration_agent_label(id: &str) -> &str {
    match id {
        "cursor" => "Cursor",
        "claude" => "Claude Code",
        "codex" => "Codex",
        "grok" => "Grok",
        "pi" => "Pi",
        other => other,
    }
}

fn integration_updates_text(updates: &[String], lang: Lang) -> String {
    if updates.len() == 1 {
        return i18n::tr(lang, "tray.integrationUpdateOne")
            .replace("{agent}", integration_agent_label(&updates[0]));
    }
    i18n::tr(lang, "tray.integrationUpdateMany").replace("{count}", &updates.len().to_string())
}

/// 分钟级「多久之前」文案（托盘渠道故障行用；分钟级粒度避免秒级微变触发菜单刷新）。
fn fmt_ago(at_ms: u64, lang: Lang) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mins = now.saturating_sub(at_ms) / 60_000;
    if mins < 1 {
        i18n::tr(lang, "tray.justNow").to_string()
    } else if mins < 60 {
        i18n::tr(lang, "tray.minutesAgo").replace("{n}", &mins.to_string())
    } else {
        i18n::tr(lang, "tray.hoursAgo").replace("{n}", &(mins / 60).to_string())
    }
}

fn fmt_uptime(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{}s", secs)
    }
}

/// 生成「期望的托盘菜单节点列表」（声明式，spec D7）：状态区只读条目 + 操作区可点条目。
/// 每个节点带稳定 `key`（可点条目的 `key` 即事件路由 id）；由 `TrayMenu::apply` diff 应用——
/// 文字变化只 `set_text`、结构变化才最小增删，绝不整段重建（整段重建会关掉已展开菜单）。
fn build_specs(
    up: bool,
    lang: Lang,
    data: &TrayData,
    integration_updates: &[String],
    lifecycle_on: bool,
    stale: bool,
    update_action: &UpdateActionState,
) -> Vec<Node> {
    let mut nodes: Vec<Node> = Vec::new();

    // —— 状态区（只读）——
    let title = if up {
        i18n::tr(lang, "tray.running").to_string()
    } else {
        i18n::tr(lang, "tray.stopped").to_string()
    };
    nodes.push(Node::item("st.title", title, false));
    if up {
        nodes.push(Node::item(
            "st.version",
            i18n::tr(lang, "tray.version").replace("{v}", &data.version),
            false,
        ));
    }
    // Keep the running daemon version truthful, then explain directly underneath why it can still
    // be old after the new binary was installed. The exact reason comes from daemon drain state.
    if data.pending {
        nodes.push(Node::item(
            "st.update_pending",
            pending_update_text(up, data, lang),
            false,
        ));
    }
    if up {
        nodes.push(Node::item(
            "st.uptime",
            i18n::tr(lang, "tray.uptime").replace("{d}", &fmt_uptime(data.uptime_secs)),
            false,
        ));
        // A pending update has a more specific drain explanation immediately below the version.
        // Keep this generic row for manual stop/restart drains.
        if data.draining && !data.pending {
            nodes.push(Node::item(
                "st.draining",
                i18n::tr(lang, "tray.draining").to_string(),
                false,
            ));
        }
        // 忙闲数量不再单列只读行——已合并进操作区「Agent 状态」入口的标题（见下方 open_agents）。
        if !data.im_connections.is_empty() {
            nodes.push(Node::item(
                "st.im",
                i18n::tr(lang, "tray.imConnections")
                    .replace("{list}", &data.im_connections.join(", ")),
                false,
            ));
        }
        // 渠道故障警示（R7）：逐渠道一行，可点击 → 打开设置渠道 tab 看错误详情。
        for issue in &data.channel_issues {
            nodes.push(Node::item(
                format!("chissue:{}", issue.channel),
                i18n::tr(lang, "tray.channelIssue")
                    .replace(
                        "{ch}",
                        &crate::autochannel::channel_label(&issue.channel, lang),
                    )
                    .replace("{t}", &fmt_ago(issue.at_ms, lang)),
                true,
            ));
        }
    }
    if data.update_available {
        nodes.push(Node::item(
            "st.update_avail",
            i18n::tr(lang, "tray.updateAvailable").replace("{v}", &data.update_latest),
            false,
        ));
    }
    if !integration_updates.is_empty() {
        nodes.push(Node::item(
            "integration_updates",
            integration_updates_text(integration_updates, lang),
            true,
        ));
    }

    // —— 操作区 ——
    nodes.push(Node::separator("sep.actions"));
    // 「待答」放在操作区最前、独立一段——它是唯一可点的状态项，混在上方一堆灰色只读行里显得乱。
    if up && data.active_requests > 0 {
        let title = i18n::tr(lang, "tray.pendingQuestions")
            .replace("{n}", &data.active_requests.to_string());
        // 有逐条摘要 → 子菜单（点击聚焦对应弹窗）；缺摘要（旧 daemon）→ 退回只读计数行。
        if data.pending_requests.is_empty() {
            nodes.push(Node::item("st.pending_count", title, false));
        } else {
            let children = data
                .pending_requests
                .iter()
                .map(|p| {
                    let label = if p.preview.is_empty() {
                        i18n::tr(lang, "tray.pendingUntitled").to_string()
                    } else {
                        p.preview.clone()
                    };
                    Node::item(format!("focus_req:{}", p.id), label, true)
                })
                .collect();
            nodes.push(Node::submenu("st.pending_menu", title, true, children));
        }
        nodes.push(Node::separator("sep.pending"));
    }
    nodes.push(Node::item(
        "open_settings",
        i18n::tr(lang, "tray.openSettings").to_string(),
        true,
    ));
    nodes.push(Node::item(
        "open_history",
        i18n::tr(lang, "tray.openHistory").to_string(),
        true,
    ));
    nodes.push(Node::item(
        "open_todos",
        i18n::tr(lang, "tray.openTodos").to_string(),
        true,
    ));
    // Show New Agent Task when the current platform has a supported terminal. This does not
    // require agentTasks to be enabled. With lifecycle tracking it moves into the Agent section,
    // 用户定案 2026-07-25）；未开启（无 Agent 区）时留在窗口区兜底。
    let new_task_available = crate::integrations::agent_launch::terminal_available();
    if new_task_available && !lifecycle_on {
        nodes.push(Node::item(
            "open_new_task",
            i18n::tr(lang, "tray.newTask").to_string(),
            true,
        ));
    }
    // Agent 区仅在开启了生命周期追踪时显示——否则窗口必为空，徒增困惑。
    // 独立成组（用户验收反馈，spec gui-agent-console 反馈记录）：分隔线 +
    // 「打开 Agent 状态窗口」直达项 + 忙闲概览子菜单（标签即「工作中 w · 空闲 i」，
    // 逐 agent：在控制台查看 / 发送消息 / 待办 / 聚焦终端；工作中在前）+
    // 「新建 Agent 任务」末项。无活动 agent / 旧 daemon（缺摘要）→ 无概览子菜单。
    if lifecycle_on {
        nodes.push(Node::separator("sep.agents_section"));
        nodes.push(Node::item(
            "open_agents",
            i18n::tr(lang, "tray.openAgents").to_string(),
            true,
        ));
        if up && !data.agents.is_empty() {
            let mut children: Vec<Node> = Vec::new();
            for a in &data.agents {
                // Agent 条目前缀用与 /watch 卡片一致的状态圆点，避免仅靠排序区分工作中/空闲。
                // 编号可直接用于 `/msg <编号>`；标题截断 24 字符防菜单过宽。
                let project = if a.project_name.is_empty() {
                    i18n::tr(lang, "autoChannel.noProject").to_string()
                } else {
                    a.project_name.clone()
                };
                let session_title = if a.title.trim().is_empty() {
                    i18n::tr(lang, "autoChannel.noTitle").to_string()
                } else {
                    truncate_chars(a.title.trim(), AGENT_TITLE_MAX_CHARS)
                };
                let fork_prefix = a
                    .forked_from_seq
                    .map(|parent_seq| {
                        format!(
                            "{} · ",
                            i18n::tr(lang, "watch.forkedFrom")
                                .replace("{id}", &format!("#{parent_seq}"))
                        )
                    })
                    .unwrap_or_default();
                let title = format!(
                    "{} · [{}] {}{} — {}（{}）",
                    agent_state_label(&a.state, lang),
                    a.seq,
                    fork_prefix,
                    agent_kind_label(&a.kind),
                    session_title,
                    project
                );
                let mut sub: Vec<Node> = Vec::new();
                // 「在控制台查看」（spec gui-agent-console C10/R4）：打开控制台并定位该会话。
                sub.push(Node::item(
                    format!("goto:{}", a.session_id),
                    i18n::tr(lang, "tray.agentOpenConsole").to_string(),
                    true,
                ));
                if a.forkable {
                    sub.push(Node::item(
                        format!("fork:{}", a.session_id),
                        i18n::tr(lang, "tray.agentFork").to_string(),
                        true,
                    ));
                }
                // 「发送消息」：grok 无可靠传话通道（首期排除，spec agent-interject D1），且仅「工作中」
                // 才显示——插话在 agent 下一次工具调用时送达，对空闲无意义（用户定案）。
                if a.kind != "grok" && a.state == "working" {
                    sub.push(Node::item(
                        format!("ijask:{}", a.session_id),
                        i18n::tr(lang, "tray.agentAskNow").to_string(),
                        true,
                    ));
                    let text = if a.pending_interject {
                        i18n::tr(lang, "tray.agentSendMessagePending").to_string()
                    } else {
                        i18n::tr(lang, "tray.agentSendMessage").to_string()
                    };
                    sub.push(Node::item(format!("ij:{}", a.session_id), text, true));
                }
                // 「添加待办」：打开待办窗口并预选该 agent 的项目（需 cwd 才能归属项目）。
                if a.cwd.as_deref().is_some_and(|c| !c.is_empty()) {
                    sub.push(Node::item(
                        format!("todo:{}", a.session_id),
                        i18n::tr(lang, "tray.agentAddTodo").to_string(),
                        true,
                    ));
                }
                if a.focusable {
                    sub.push(Node::item(
                        format!("term:{}", a.session_id),
                        i18n::tr(lang, "tray.agentFocusTerminal").to_string(),
                        true,
                    ));
                }
                if sub.is_empty() {
                    // 无任何可用动作（grok 且终端不可聚焦）：列为只读行，仅供感知。
                    children.push(Node::item(format!("agent:{}", a.session_id), title, false));
                } else {
                    children.push(Node::submenu(
                        format!("agent:{}", a.session_id),
                        title,
                        true,
                        sub,
                    ));
                }
            }
            let overview = i18n::tr(lang, "tray.agentOverview")
                .replace("{w}", &data.agents_working.to_string())
                .replace("{i}", &data.agents_idle.to_string());
            nodes.push(Node::submenu("agents_menu", overview, true, children));
        }
        if new_task_available {
            nodes.push(Node::item(
                "open_new_task",
                i18n::tr(lang, "tray.newTask").to_string(),
                true,
            ));
        }
    }
    nodes.push(Node::separator("sep.update"));
    if let Some(text) = update_action_text(update_action, lang) {
        nodes.push(Node::item("st.update_action", text, false));
    }
    let update_busy = update_action.busy();
    nodes.push(Node::item(
        "check_update",
        i18n::tr(lang, "tray.checkUpdate").to_string(),
        !update_busy,
    ));
    if data.update_available {
        let (id, text) =
            available_update_action(crate::update::apply_mode(), lang, &data.update_latest);
        nodes.push(Node::item(id, text, !update_busy));
    }
    // 盘上二进制已换新但窗口开着（自动换新被挡）→ 用户可主动重启宿主完成更新（B2）。
    if stale {
        nodes.push(Node::item(
            "host_restart",
            i18n::tr(lang, "tray.restartHost").to_string(),
            true,
        ));
    }
    nodes.push(Node::separator("sep.daemon"));
    if up {
        nodes.push(Node::item(
            "restart_daemon",
            i18n::tr(lang, "tray.restartDaemon").to_string(),
            true,
        ));
        // 有「工作中」agent 时「停止」无意义：daemon 一停，agent 的生命周期 hook（report_agent_event
        // → ensure_running）或下次 ask 会几秒内把它重新拉起。故隐藏停止项，仅留一行灰色说明。
        if data.agents_working > 0 {
            nodes.push(Node::item(
                "st.stop_blocked",
                i18n::tr(lang, "tray.stopDaemonBlocked").to_string(),
                false,
            ));
        } else {
            nodes.push(Node::item(
                "stop_daemon",
                i18n::tr(lang, "tray.stopDaemon").to_string(),
                true,
            ));
        }
    } else {
        nodes.push(Node::item(
            "start_daemon",
            i18n::tr(lang, "tray.startDaemon").to_string(),
            true,
        ));
    }
    nodes
}

fn available_update_action(
    mode: crate::update::UpdateApplyMode,
    lang: Lang,
    version: &str,
) -> (&'static str, String) {
    let (id, key) = if mode == crate::update::UpdateApplyMode::Automatic {
        ("apply_update", "tray.applyUpdate")
    } else {
        ("prepare_update", "tray.prepareManualUpdate")
    };
    (id, i18n::tr(lang, key).replace("{v}", version))
}

fn pending_update_text(up: bool, data: &TrayData, lang: Lang) -> String {
    let version = if data.update_latest.trim().is_empty() {
        i18n::tr(lang, "tray.updateTargetFallback").to_string()
    } else {
        format!("v{}", data.update_latest)
    };
    let key = if !up {
        "tray.updatePendingNextStart"
    } else if data.draining && data.active_requests > 0 {
        "tray.updatePendingWaiting"
    } else if data.draining {
        "tray.updatePendingRestarting"
    } else {
        "tray.updatePendingSwitching"
    };
    i18n::tr(lang, key)
        .replace("{v}", &version)
        .replace("{n}", &data.active_requests.to_string())
}

fn update_action_text(action: &UpdateActionState, lang: Lang) -> Option<String> {
    let text = match action {
        UpdateActionState::Idle => return None,
        UpdateActionState::Checking => i18n::tr(lang, "tray.checkingUpdate").to_string(),
        UpdateActionState::Current => i18n::tr(lang, "tray.updateCurrent").to_string(),
        UpdateActionState::Found(version) => {
            i18n::tr(lang, "tray.updateFound").replace("{v}", version)
        }
        UpdateActionState::CheckFailed(error) => {
            i18n::tr(lang, "tray.checkUpdateFailed").replace("{e}", error)
        }
        UpdateActionState::Applying => i18n::tr(lang, "tray.applyingUpdate").to_string(),
        UpdateActionState::ApplyFailed(error) => {
            i18n::tr(lang, "tray.applyUpdateFailed").replace("{e}", error)
        }
        UpdateActionState::RestartFailed(error) => {
            i18n::tr(lang, "tray.restartHostFailed").replace("{e}", error)
        }
    };
    Some(text)
}

fn compact_update_error(error: &str) -> String {
    let one_line = error.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&one_line, 72)
}

/// agent 家族展示名（托盘 Agent 子菜单标签用；与 `AgentKind::label` 同口径）。
fn agent_kind_label(kind: &str) -> &str {
    match crate::agents::AgentKind::parse(kind) {
        Some(k) => k.label(),
        None => kind,
    }
}

/// 托盘 Agent 条目的状态前缀；复用 /watch 卡片的状态文案与圆点。
fn agent_state_label(state: &str, lang: Lang) -> &'static str {
    match state {
        "working" => i18n::tr(lang, "watch.stateWorking"),
        _ => i18n::tr(lang, "watch.stateIdle"),
    }
}

/// Agent 子菜单条目里会话标题的截断长度（与「待答」预览同 24 字符口径）。
const AGENT_TITLE_MAX_CHARS: usize = 24;

/// 按 Unicode 字符截断，超出追加省略号（与 `daemon::request::truncate_chars` 同逻辑）。
fn truncate_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

/// 托盘菜单事件分派（由 launch() 的全局 `on_menu_event` 在宿主进程中调用）。
pub fn on_menu_event(app: &AppHandle, id: &str) {
    // 待答子菜单项：聚焦对应弹窗（向 daemon 发 FocusRequest，由 daemon 转发到该请求的弹窗进程）。
    if let Some(request_id) = id.strip_prefix("focus_req:") {
        let request_id = request_id.to_string();
        tauri::async_runtime::spawn(async move {
            if let Ok(stream) = transport::connect().await {
                let (_r, mut w) = stream.into_split();
                let _ = ipc::write_msg(&mut w, &ClientMsg::FocusRequest { request_id }).await;
            }
        });
        return;
    }
    // Agent 子菜单「在控制台查看」：打开（或聚焦）控制台并定位该会话（spec gui-agent-console C10/R4）。
    if let Some(session_id) = id.strip_prefix("goto:") {
        open_window(
            app,
            WindowKind::Agents,
            false,
            None,
            Some(crate::gui_host::InterjectTarget {
                session: session_id.to_string(),
                agent: None,
                cwd: None,
            }),
            None,
            None,
        );
        return;
    }
    // Agent 子菜单「发送消息」：宿主本进程直接开（或聚焦）该 session 的插话 composer 窗口。
    if let Some(session_id) = id.strip_prefix("ij:") {
        let info = app.try_state::<HostState>().and_then(|s| {
            s.data
                .lock()
                .unwrap()
                .agents
                .iter()
                .find(|a| a.session_id == session_id)
                .cloned()
        });
        if let Some(a) = info {
            open_window(
                app,
                WindowKind::Interject,
                false,
                None,
                Some(crate::gui_host::InterjectTarget {
                    session: a.session_id,
                    agent: Some(a.kind),
                    cwd: a.cwd,
                }),
                None,
                None,
            );
        }
        return;
    }
    // Agent 子菜单「要求提问」：追加一条固定插话，不打开 composer、不覆盖已有待送达内容。
    if let Some(session_id) = id.strip_prefix("ijask:") {
        let session_id = session_id.to_string();
        tauri::async_runtime::spawn(async move {
            if let Ok(stream) = transport::connect().await {
                let (_r, mut w) = stream.into_split();
                let _ = ipc::write_msg(
                    &mut w,
                    &ClientMsg::InterjectAppend {
                        session_id,
                        text: QUICK_ASK_INTERJECT.to_string(),
                        attachments: Vec::new(),
                    },
                )
                .await;
            }
        });
        return;
    }
    // 渠道故障警示行（R7）：打开设置并定位到渠道 tab（错误详情显示在渠道卡片上）。
    if id.starts_with("chissue:") {
        open_window_settings_tab(app, "channel");
        return;
    }
    if id == "integration_updates" {
        open_window_settings_tab(app, "integration");
        return;
    }
    // Agent 子菜单「添加待办」：打开（或聚焦）待办窗口并预选该 agent 的项目（cwd → git 根）。
    // 前端收到预选项目即自动聚焦新增输入框。项目解析失败时仍开窗（由前端自选默认）。
    if let Some(session_id) = id.strip_prefix("todo:") {
        let cwd = app.try_state::<HostState>().and_then(|s| {
            s.data
                .lock()
                .unwrap()
                .agents
                .iter()
                .find(|a| a.session_id == session_id)
                .and_then(|a| a.cwd.clone())
        });
        let project = cwd
            .map(|c| crate::project::detect_from(std::path::Path::new(&c)))
            .filter(|k| !k.is_empty());
        open_window(app, WindowKind::Todos, false, project, None, None, None);
        return;
    }
    if let Some(session_id) = id.strip_prefix("fork:") {
        open_window(
            app,
            WindowKind::ForkTask,
            false,
            None,
            Some(crate::gui_host::InterjectTarget {
                session: session_id.to_string(),
                agent: None,
                cwd: None,
            }),
            None,
            None,
        );
        return;
    }
    // Agent 子菜单「聚焦终端」：AppleScript 可能阻塞（授权弹窗等），放后台线程。
    if let Some(session_id) = id.strip_prefix("term:") {
        let focus = app.try_state::<HostState>().and_then(|s| {
            s.data
                .lock()
                .unwrap()
                .agents
                .iter()
                .find(|a| a.session_id == session_id)
                .map(|a| (a.pid, a.launch_id.clone()))
        });
        if let Some((pid, launch_id)) = focus {
            std::thread::spawn(move || {
                let _ = crate::integrations::terminal_focus::focus_agent_terminal(
                    pid,
                    launch_id.as_deref(),
                );
            });
        }
        return;
    }
    match id {
        "open_settings" => open_window(app, WindowKind::Settings, false, None, None, None, None),
        // 托盘「历史」无调用方项目上下文 → 默认展示全部项目。
        "open_history" => open_window(app, WindowKind::History, true, None, None, None, None),
        "open_agents" => open_window(app, WindowKind::Agents, false, None, None, None, None),
        // 托盘「待办」无项目上下文 → 由前端自选默认项目。
        "open_todos" => open_window(app, WindowKind::Todos, false, None, None, None, None),
        // 托盘「新建 Agent 任务」（spec gui-agent-task-launch）：无预选打开通用表单。
        "open_new_task" => open_window(app, WindowKind::NewTask, false, None, None, None, None),
        "check_update" => {
            let Some(state) = app.try_state::<HostState>() else {
                return;
            };
            {
                let mut action = state.update_action.lock().unwrap();
                if action.busy() {
                    return;
                }
                *action = UpdateActionState::Checking;
            }
            refresh_on_main(app);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                match crate::update::check_fresh().await {
                    Ok(info) => {
                        let info = crate::update::persist_check_result(info, true);
                        sync_checked_update(&app, &info, true);
                        crate::client::notify_update_state_changed().await;
                    }
                    Err(error) => {
                        sync_update_check_error(&app, &error.to_string());
                    }
                }
            });
        }
        "apply_update" => {
            let Some(state) = app.try_state::<HostState>() else {
                return;
            };
            if let Err(error) = crate::update::ensure_automatic_apply_allowed() {
                *state.update_action.lock().unwrap() =
                    UpdateActionState::ApplyFailed(compact_update_error(&error.to_string()));
                refresh_on_main(app);
                return;
            }
            {
                let mut action = state.update_action.lock().unwrap();
                if action.busy() {
                    return;
                }
                *action = UpdateActionState::Applying;
            }
            refresh_on_main(app);
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let updater = crate::update::select_updater();
                match updater.apply(None).await {
                    Ok(()) => {
                        crate::update::state::set_pending(true);
                        if let Some(state) = app.try_state::<HostState>() {
                            let mut data = state.data.lock().unwrap();
                            data.update_available = false;
                            data.pending = true;
                            *state.update_action.lock().unwrap() = UpdateActionState::Idle;
                        }
                        refresh_on_main(&app);
                        // Persisted state tells every open surface that the update is pending; the
                        // Hello fingerprint check immediately starts graceful daemon replacement.
                        crate::client::notify_update_applied().await;
                        refresh_binary_on_main(&app);
                    }
                    Err(error) => {
                        if let Some(state) = app.try_state::<HostState>() {
                            *state.update_action.lock().unwrap() = UpdateActionState::ApplyFailed(
                                compact_update_error(&error.to_string()),
                            );
                        }
                        refresh_on_main(&app);
                    }
                }
            });
        }
        // Settings owns the second confirmation and preserves the exact Direct/npm next step
        // before starting the helper that will close this GUI Host.
        "prepare_update" => open_window(
            app,
            WindowKind::Settings,
            false,
            Some("general#manual-update".to_string()),
            None,
            None,
            None,
        ),
        // B2：用户主动重启宿主完成二进制换新（窗口开着也重启——用户已知情选择）。
        "host_restart" => {
            restart_host(app);
        }
        "start_daemon" => {
            tauri::async_runtime::spawn(async {
                let _ = crate::client::ensure_running().await;
            });
        }
        "restart_daemon" => {
            tauri::async_runtime::spawn(async {
                let _ = crate::client::request_stop(false).await;
                crate::client::wait_until_down(Duration::from_secs(5)).await;
                let _ = crate::client::ensure_running().await;
            });
        }
        "stop_daemon" => {
            tauri::async_runtime::spawn(async {
                let _ = crate::client::request_stop(false).await;
            });
        }
        _ => {}
    }
}

// ===== 窗口管理 =====

/// 在宿主进程内打开（或聚焦）指定窗口，并刷新窗口计数 / 续命。须在主线程调用。
/// `param` 按窗口类型复用：历史窗口 = 调用方项目 key（默认过滤到该项目，None 用宿主自身项目）；
/// 设置窗口 = 初始定位 tab（如 "channel"，None 用默认 tab）；待办/新建任务窗口 = 预选项目 key。
/// `target.session` 用于 Agent 窗口预选或插话窗口唯一键；`target.agent/cwd` 仅插话窗口使用；
/// `todo` 仅新建任务窗口使用。
pub(crate) fn open_window(
    app: &AppHandle,
    kind: WindowKind,
    all: bool,
    param: Option<String>,
    target: Option<crate::gui_host::InterjectTarget>,
    todo: Option<String>,
    history_target: Option<crate::gui_host::HistoryOpenTarget>,
) {
    let cfg = AppConfig::load_without_secrets();
    // 弹窗在「另一个进程」（daemon 拉起的助手），宿主无 popup 窗口可探测；改据 daemon 在途请求数
    // 判定：置顶开启且有在途请求（即有弹窗在屏）→ 让辅助窗口与弹窗同级，浮于其上。
    let pin_above_popup = cfg.general.always_on_top
        && app
            .try_state::<HostState>()
            .map(|s| s.data.lock().unwrap().active_requests > 0)
            .unwrap_or(false);
    let r = match kind {
        WindowKind::Settings => {
            crate::app::create_settings_window(app, &cfg, pin_above_popup, param.as_deref())
        }
        WindowKind::History => crate::app::create_history_window(
            app,
            &cfg,
            all,
            param.as_deref(),
            history_target.as_ref(),
            pin_above_popup,
        ),
        WindowKind::Agents => crate::app::create_agents_window(
            app,
            &cfg,
            target.as_ref().map(|t| t.session.as_str()),
            pin_above_popup,
        ),
        WindowKind::Interject => match &target {
            Some(t) => crate::app::create_interject_window(app, &cfg, t, pin_above_popup),
            None => return, // session 缺失：无法定位目标 agent，忽略。
        },
        // `param` 槽位在待办窗口语义下是「预选项目 key」（spec todo-whats-next D9）。
        WindowKind::Todos => {
            crate::app::create_todos_window(app, &cfg, param.as_deref(), pin_above_popup)
        }
        // `param` = 预选项目 key、`todo` = 预选待办 id（spec gui-agent-task-launch）。
        WindowKind::NewTask => crate::app::create_new_task_window(
            app,
            &cfg,
            param.as_deref(),
            todo.as_deref(),
            pin_above_popup,
        ),
        WindowKind::ForkTask => match &target {
            Some(t) => crate::app::create_fork_task_window(app, &cfg, &t.session, pin_above_popup),
            None => return,
        },
    };
    if r.is_ok() {
        // 宿主是 accessory app（不自动激活）：新建窗口需显式聚焦，才能前置到置顶弹窗之上并接收键盘。
        let label = match kind {
            WindowKind::Settings => "settings".to_string(),
            WindowKind::History => "history".to_string(),
            WindowKind::Agents => "agents".to_string(),
            WindowKind::Todos => "todos".to_string(),
            WindowKind::NewTask => "newtask".to_string(),
            WindowKind::ForkTask => "fork-task".to_string(),
            WindowKind::Interject => target
                .as_ref()
                .map(|t| crate::gui_host::interject_label(&t.session))
                .unwrap_or_default(),
        };
        if let Some(w) = app.get_webview_window(&label) {
            let _ = w.set_focus();
        }
        // Agent 订阅**不在此处启动**：必须等前端注册好 `agents-updated` 监听后再经命令触发
        // （`start_agents_subscription` → `restart_agents_subscription`），否则 daemon 的首帧
        // 立即快照会早于监听而丢失，导致窗口长时间停在 Loading。
    }
    recount_windows(app);
}

/// 打开（或聚焦）设置窗口并定位到指定 tab（R7 托盘渠道故障行使用）。
/// 已开窗 → `create_settings_window` 内经 `settings-goto-tab` 事件切 tab；新开 → tab 进初始 URL。
fn open_window_settings_tab(app: &AppHandle, tab: &str) {
    open_window(
        app,
        WindowKind::Settings,
        false,
        Some(tab.to_string()),
        None,
        None,
        None,
    );
}

/// 重算宿主承载的窗口数，并据此维护续命连接与退出判定。可在任意线程调用（只读窗口表 + 原子）。
pub fn recount_windows(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let n = app
        .webview_windows()
        .keys()
        .filter(|l| is_hosted_label(l))
        .count();
    state.windows_open.store(n, Ordering::SeqCst);
    if n > 0 {
        state.ever_open.store(true, Ordering::SeqCst);
    }
    // Agent 窗口已关 → 停订阅（释放 daemon 连接，避免长命宿主借订阅一直把 daemon 续命）。
    if app.get_webview_window("agents").is_none() {
        stop_agents_subscription(app);
    }
    update_keepalive(app);
    // B1：最后一个窗口关闭时立即检查二进制换新（不等 15s 轮询），避免长命宿主拿旧版
    // 继续服务。off 模式除外——下方 evaluate_exit 本来就会退出，re-exec 徒增一次空跑。
    if n == 0 && state.mode() != MenuBarIconMode::Off {
        maybe_refresh_binary(app);
    }
    evaluate_exit(app);
}

/// （重）启动 agent 状态订阅：停掉旧的，再开一条新订阅。由前端在监听就绪后经命令触发——重启可让
/// daemon 重推一帧立即快照，从而每次开窗都能立刻拿到数据（长命宿主复用旧订阅则会首屏长 Loading）。
pub fn restart_agents_subscription(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let mut slot = state.agents_sub.lock().unwrap();
    if let Some(old) = slot.take() {
        old.notify_one(); // 停旧订阅任务
    }
    let stop = Arc::new(Notify::new());
    *slot = Some(stop.clone());
    crate::app::spawn_agents_subscription(app.clone(), Some(stop));
}

/// 停掉 agent 状态订阅（agent 窗口关闭时调用）。
pub fn stop_agents_subscription(app: &AppHandle) {
    if let Some(state) = app.try_state::<HostState>() {
        if let Some(old) = state.agents_sub.lock().unwrap().take() {
            old.notify_one();
        }
    }
}

/// 维护「窗口期续命连接」（spec D5）：有窗口且 daemon 在跑 → 持一条普通连接（计入 daemon active）；
/// 无窗口或 daemon 不在 → 关闭。
fn update_keepalive(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let windows = state.windows_open.load(Ordering::SeqCst);
    let up = state.daemon_up.load(Ordering::SeqCst);
    let mut ka = state.keepalive.lock().unwrap();
    if windows > 0 && up && ka.is_none() {
        let stop = Arc::new(Notify::new());
        *ka = Some(stop.clone());
        tauri::async_runtime::spawn(keepalive_task(stop));
    } else if (windows == 0 || !up) && ka.is_some() {
        if let Some(stop) = ka.take() {
            stop.notify_one();
        }
    }
}

/// 续命连接任务：开一条普通连接给 daemon（不发消息，纯占位计入 active），直到被通知停止或 daemon 断开。
async fn keepalive_task(stop: Arc<Notify>) {
    // 仅给「正在运行的」daemon 续命：用普通 connect（不 ensure_running），连不上即不续命。
    if let Ok(stream) = transport::connect().await {
        let (r, _w) = stream.into_split();
        let mut reader = BufReader::new(r);
        tokio::select! {
            _ = stop.notified() => {}
            _ = async {
                // daemon 主动断开（如换新退出）→ 读到 EOF 即结束。
                while let Ok(Some(_)) = ipc::read_msg::<_, ServerMsg>(&mut reader).await {}
            } => {}
        }
        // stream 在此 drop → 连接关闭 → daemon active -= 1（重新计时空闲退出）。
    }
}

/// 宿主退出判定（spec D4/D5/§5.4）。可在任意线程调用。
fn evaluate_exit(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    if state.windows_open.load(Ordering::SeqCst) > 0 {
        return; // 有窗口绝不退出。
    }
    let settled = state.ever_open.load(Ordering::SeqCst) || state.grace_over.load(Ordering::SeqCst);
    match state.mode() {
        // off：宿主仅为窗口而生，窗口都关了即退出。
        MenuBarIconMode::Off => {
            if settled {
                app.exit(0);
            }
        }
        // active：daemon 断连且无窗口 → 图标消失、宿主退出。
        MenuBarIconMode::Active => {
            if settled && !state.daemon_up.load(Ordering::SeqCst) {
                app.exit(0);
            }
        }
        // always：常驻（图标转停止态，不退出）。
        MenuBarIconMode::Always => {}
    }
}

// ===== 自有 IPC 监听（gui-host.sock）=====

fn start_ipc_listener(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        let listener = match crate::gui_host::bind() {
            Ok(l) => l,
            Err(_) => return,
        };
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let app = app.clone();
                    tauri::async_runtime::spawn(handle_host_conn(stream, app));
                }
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                }
            }
        }
    });
}

async fn handle_host_conn(stream: transport::Stream, app: AppHandle) {
    let (r, mut w) = stream.into_split();
    let mut reader = BufReader::new(r);
    while let Ok(Some(msg)) = ipc::read_msg::<_, HostMsg>(&mut reader).await {
        match msg {
            HostMsg::OpenWindow {
                kind,
                all,
                project,
                session,
                agent,
                cwd,
                todo,
                history_target,
            } => {
                // 回执（让客户端确认已受理），再到主线程开窗。
                let _ = ipc::write_msg(&mut w, &HostMsg::Ping).await;
                let target = session.map(|session| crate::gui_host::InterjectTarget {
                    session,
                    agent,
                    cwd,
                });
                let app2 = app.clone();
                let _ = app.run_on_main_thread(move || {
                    open_window(&app2, kind, all, project, target, todo, history_target)
                });
            }
            HostMsg::Ping => {
                let _ = ipc::write_msg(&mut w, &HostMsg::Ping).await;
            }
            HostMsg::Shutdown => {
                // Confirm that the shutdown frame was consumed before the client starts checking
                // for pipe disappearance. This removes a named-pipe close race on Windows.
                let _ = ipc::write_msg(&mut w, &HostMsg::Ping).await;
                // `AppHandle::exit` can wait for a Settings update-check task (HTTP timeout 30s),
                // keeping the Windows executable locked. This is a cooperative Host-only shutdown
                // after daemon drain: release Tauri resources, then terminate this process.
                app.cleanup_before_exit();
                std::process::exit(0);
            }
        }
    }
}

// ===== daemon 状态订阅（非保活）=====

fn start_status_subscription(app: AppHandle) {
    // 事件驱动重连信号：daemon metadata 出现/变化（daemon 起停）即唤醒下方循环立即重连，
    // 取代「daemon 关着时每 2s 盲连」的忙轮询。配 30s 兜底超时防漏事件。
    let state_event = Arc::new(Notify::new());
    spawn_daemon_state_watch(state_event.clone());
    tauri::async_runtime::spawn(async move {
        loop {
            // off 模式无托盘，不必订阅；等 state 事件或 2s 复查模式（覆盖运行时切到 active/always）。
            if mode_of(&app) == MenuBarIconMode::Off {
                tokio::select! {
                    _ = state_event.notified() => {}
                    _ = tokio::time::sleep(Duration::from_secs(2)) => {}
                }
                continue;
            }
            if let Ok(stream) = transport::connect().await {
                let (r, mut w) = stream.into_split();
                let mut reader = BufReader::new(r);
                if ipc::write_msg(&mut w, &ClientMsg::TraySubscribe)
                    .await
                    .is_ok()
                {
                    set_daemon_up(&app, true);
                    loop {
                        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
                            Ok(Some(ServerMsg::TrayState {
                                running,
                                version,
                                uptime_secs,
                                active_requests,
                                im_connections,
                                draining,
                                agents_working,
                                agents_idle,
                                update_available,
                                update_latest,
                                pending,
                                pending_requests,
                                agents,
                                channel_issues,
                            })) => {
                                let persisted = crate::update::state::load();
                                let (update_available, update_latest, pending) =
                                    merge_update_status(
                                        update_available,
                                        update_latest,
                                        pending,
                                        &persisted,
                                        &crate::update::current_version(),
                                    );
                                if let Some(state) = app.try_state::<HostState>() {
                                    *state.data.lock().unwrap() = TrayData {
                                        running,
                                        version,
                                        uptime_secs,
                                        active_requests,
                                        im_connections,
                                        draining,
                                        agents_working,
                                        agents_idle,
                                        update_available,
                                        update_latest,
                                        pending,
                                        pending_requests,
                                        agents,
                                        channel_issues,
                                    };
                                }
                                refresh_on_main(&app);
                                maybe_refresh_binary(&app);
                            }
                            Ok(Some(_)) => {} // 忽略未知 / 其它变体（兼容）。
                            Ok(None) | Err(_) => break,
                        }
                    }
                }
            }
            // 断连：daemon 空闲退出 / 停止 / 换新。刷新会因签名不变而仅在「运行→停止」首刷生效。
            set_daemon_up(&app, false);
            refresh_on_main(&app);
            evaluate_exit(&app);
            // 事件驱动重连：等 daemon metadata 出现/变化即重连，30s 兜底防漏事件。
            tokio::select! {
                _ = state_event.notified() => {}
                _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            }
        }
    });
}

fn daemon_state_signal_path() -> std::path::PathBuf {
    crate::daemon::lifecycle::meta_path()
}

/// Watch the cross-platform daemon metadata file. Unlike the transport endpoint, this is a real
/// filesystem path on both Unix and Windows, and its create/remove lifecycle mirrors daemon start
/// and stop closely enough to wake the status subscription immediately.
fn spawn_daemon_state_watch(event: Arc<Notify>) {
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        use std::sync::mpsc::channel;
        let state_path = daemon_state_signal_path();
        let Some(name) = state_path.file_name().map(|n| n.to_os_string()) else {
            return;
        };
        let Some(dir) = state_path.parent().map(|d| d.to_path_buf()) else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = channel::<()>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if ev
                        .paths
                        .iter()
                        .any(|p| p.file_name() == Some(name.as_os_str()))
                    {
                        let _ = tx.send(());
                    }
                }
            }) {
                Ok(w) => w,
                Err(_) => return,
            };
        if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
            return;
        }
        // 收到事件即唤醒订阅循环（Notify 会暂存一个许可，避免错过唤醒）。
        while rx.recv().is_ok() {
            event.notify_one();
        }
    });
}

fn mode_of(app: &AppHandle) -> MenuBarIconMode {
    app.try_state::<HostState>()
        .map(|s| s.mode())
        .unwrap_or(MenuBarIconMode::Off)
}

fn set_daemon_up(app: &AppHandle, up: bool) {
    let mut became_up = false;
    if let Some(state) = app.try_state::<HostState>() {
        became_up = up && !state.daemon_up.swap(up, Ordering::SeqCst);
    }
    if became_up {
        refresh_integration_updates(app);
    }
    update_keepalive(app);
}

fn refresh_on_main(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || refresh_tray(&app2));
}

fn refresh_binary_on_main(app: &AppHandle) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        update_binary_stale(&app2);
        maybe_refresh_binary(&app2);
    });
}

// ===== 配置监听（模式 / 语言 / 登录项）=====

fn start_config_watch(app: AppHandle) {
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        use std::sync::mpsc::{channel, RecvTimeoutError};
        let dir = crate::paths::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = channel::<()>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let hit = ev
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n == "config.json").unwrap_or(false));
                    if hit {
                        let _ = tx.send(());
                    }
                }
            }) {
                Ok(w) => w,
                Err(_) => return,
            };
        if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
            return;
        }
        loop {
            if rx.recv().is_err() {
                break;
            }
            // 去抖：合并连续事件。
            loop {
                match rx.recv_timeout(Duration::from_millis(300)) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            let cfg = AppConfig::load_without_secrets();
            let app2 = app.clone();
            if app
                .run_on_main_thread(move || apply_config(&app2, &cfg))
                .is_err()
            {
                break; // app 已退出。
            }
        }
    });
}

/// 应用新配置（主线程）：语言热切换、模式切换（建/移图标 + 登录项 + 活动策略）。
fn apply_config(app: &AppHandle, cfg: &AppConfig) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    // 主题跨进程热同步：弹窗导航栏 / CLI 改主题只写配置文件，宿主窗口（设置/历史/
    // 待办/Agents/插话）靠本 watcher 跟进——原生外观 + 前端事件一起下发。
    // 设置窗口在宿主内改主题时会再触发一次，重复应用幂等无害。
    let theme = match cfg.general.theme {
        ThemeMode::Light => "light",
        ThemeMode::Dark => "dark",
        ThemeMode::System => "system",
    };
    crate::commands::apply_theme_to_windows(app, theme);
    {
        use tauri::Emitter;
        if let Ok(general) = serde_json::to_value(&cfg.general) {
            let _ = app.emit("settings-updated", general);
        }
    }
    let new_lang = Lang::resolve(&cfg.general.language);
    let new_mode = cfg.general.menu_bar_icon;
    let old_mode = state.mode();
    *state.lang.lock().unwrap() = new_lang;
    *state.mode.lock().unwrap() = new_mode;

    if new_mode != old_mode {
        apply_activation_policy(app, new_mode);
        match new_mode {
            MenuBarIconMode::Off => {
                let _ = ensure_tray(app, false);
                let _ = crate::integrations::login_item::uninstall();
                evaluate_exit(app);
            }
            MenuBarIconMode::Active => {
                let _ = crate::integrations::login_item::uninstall();
                if tray_supported() {
                    let _ = ensure_tray(app, true);
                }
            }
            MenuBarIconMode::Always => {
                let _ = crate::integrations::login_item::ensure_installed();
                if tray_supported() {
                    let _ = ensure_tray(app, true);
                }
            }
        }
    }

    // 守护进程生命周期换挡：切到「保活」即视为一次触发——立即拉起 daemon（若未运行）。
    // 登录项（开机自启）由 daemon 自身在启动 / on_config_changed 时同步（见 daemon::sync_daemon_login_item），
    // 宿主这里只负责「立即起」，避免开了开关却看不到效果。关掉保活不强杀：让 daemon 按原空闲策略自然退出。
    let new_life = cfg.general.daemon_lifecycle;
    let old_life = *state.daemon_lifecycle.lock().unwrap();
    *state.daemon_lifecycle.lock().unwrap() = new_life;
    if new_life != old_life && new_life == DaemonLifecycleMode::KeepAlive {
        tauri::async_runtime::spawn(async {
            let _ = crate::client::ensure_running().await;
        });
    }

    // 语言 / 状态变化即重建菜单。
    refresh_tray(app);
}

// ===== 二进制换新（spec D11）=====

fn start_binary_watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_secs(15)).await;
            update_binary_stale(&app);
            maybe_refresh_binary(&app);
        }
    });
}

/// 维护「盘上二进制已换新」标记；发生翻转时重绘托盘——有窗口挡住自动换新时，
/// 菜单会出现「重启以完成更新」项（B2），换新完成后自动消失。
fn update_binary_stale(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let stale = host_disk_fingerprint(&state) != state.startup_fp;
    if state.binary_stale.swap(stale, Ordering::SeqCst) != stale {
        refresh_tray(app);
    }
}

/// 盘上二进制内容变化且无打开窗口 → 换到新版（不打断在用窗口）。
fn maybe_refresh_binary(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    if state.windows_open.load(Ordering::SeqCst) > 0 {
        return;
    }
    // 以「自身盘上二进制内容是否变化」为准（pending 仅作提示）；新实例会捕获新指纹，不会循环。
    if host_disk_fingerprint(&state) == state.startup_fp {
        return;
    }
    restart_host(app);
}

fn host_disk_fingerprint(state: &HostState) -> Fingerprint {
    if state.executable_path.as_os_str().is_empty() {
        lifecycle::current_fingerprint()
    } else {
        lifecycle::fingerprint_at(&state.executable_path)
    }
}

/// 用盘上（新）二进制重启宿主：释放单实例锁；always 交 launchd KeepAlive 重启，其它自我 re-exec。
fn restart_host(app: &AppHandle) {
    let Some(state) = app.try_state::<HostState>() else {
        return;
    };
    let mode = state.mode();
    let executable_path = state.executable_path.clone();
    release_lock(); // 让新实例可抢锁。
    #[cfg(target_os = "macos")]
    {
        if mode == MenuBarIconMode::Always {
            app.exit(0);
            return;
        }
    }
    let spawned = if executable_path.as_os_str().is_empty() {
        crate::gui_host::spawn_detached()
    } else {
        crate::gui_host::spawn_detached_from(&executable_path)
    };
    match spawned {
        Ok(()) => app.exit(0),
        Err(error) => {
            // Keep serving from the in-memory old image if the new disk image could not launch.
            // Reacquiring the lock prevents another Host from racing this recovery path.
            if acquire_singleton() {
                if let Some(state) = app.try_state::<HostState>() {
                    *state.update_action.lock().unwrap() =
                        UpdateActionState::RestartFailed(compact_update_error(&error.to_string()));
                }
                refresh_on_main(app);
            } else {
                app.exit(0);
            }
        }
    }
    let _ = mode;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_status_watch_uses_real_metadata_file() {
        let path = daemon_state_signal_path();
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("daemon.json")
        );
        assert!(!path.to_string_lossy().starts_with(r"\\.\pipe\"));
    }

    fn item<'a>(nodes: &'a [Node], key: &str) -> (&'a str, bool) {
        nodes
            .iter()
            .find_map(|node| match node {
                Node::Item {
                    key: node_key,
                    text,
                    enabled,
                } if node_key == key => Some((text.as_str(), *enabled)),
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing menu item {key}"))
    }

    fn has_item(nodes: &[Node], key: &str) -> bool {
        nodes.iter().any(|node| match node {
            Node::Item { key: node_key, .. } => node_key == key,
            _ => false,
        })
    }

    #[test]
    fn tray_agent_title_marks_fork_parent_sequence() {
        let nodes = build_specs(
            true,
            Lang::Zh,
            &TrayData {
                running: true,
                agents_working: 1,
                agents: vec![ipc::TrayAgentInfo {
                    session_id: "child".to_string(),
                    seq: 6,
                    kind: "codex".to_string(),
                    title: "同一个标题".to_string(),
                    forked_from_seq: Some(5),
                    project_name: "HumanInLoop".to_string(),
                    cwd: Some("/tmp/HumanInLoop".to_string()),
                    state: "working".to_string(),
                    pending_interject: false,
                    focusable: true,
                    forkable: true,
                    pid: Some(42),
                    launch_id: None,
                }],
                ..Default::default()
            },
            &[],
            true,
            false,
            &UpdateActionState::Idle,
        );
        let agent_title = nodes
            .iter()
            .find_map(|node| match node {
                Node::Submenu { key, children, .. } if key == "agents_menu" => {
                    children.iter().find_map(|child| match child {
                        Node::Submenu { key, text, .. } if key == "agent:child" => Some(text),
                        _ => None,
                    })
                }
                _ => None,
            })
            .expect("forked agent submenu");
        assert!(agent_title.contains("[6] 从 #5 分叉 · Codex"));
    }

    #[test]
    fn cached_update_is_available_without_daemon() {
        let state = crate::update::state::UpdateState {
            latest_version: "1.2.0".to_string(),
            ..Default::default()
        };
        assert!(cached_update_available(&state, "1.1.9"));
        assert!(!cached_update_available(&state, "1.2.0"));

        let dismissed = crate::update::state::UpdateState {
            dismissed_versions: vec!["1.2.0".to_string()],
            ..state
        };
        assert!(!cached_update_available(&dismissed, "1.1.9"));
    }

    #[test]
    fn manual_check_survives_stale_daemon_snapshots() {
        let persisted = crate::update::state::UpdateState {
            latest_version: "1.2.0".to_string(),
            ..Default::default()
        };
        assert_eq!(
            merge_update_status(false, String::new(), false, &persisted, "1.1.0"),
            (true, "1.2.0".to_string(), false)
        );

        let pending = crate::update::state::UpdateState {
            pending: true,
            ..persisted.clone()
        };
        assert_eq!(
            merge_update_status(true, "1.2.0".to_string(), false, &pending, "1.1.0"),
            (false, "1.2.0".to_string(), true)
        );

        let dismissed = crate::update::state::UpdateState {
            dismissed_versions: vec!["1.2.0".to_string()],
            ..persisted
        };
        assert_eq!(
            merge_update_status(true, "1.1.5".to_string(), false, &dismissed, "1.1.0"),
            (false, "1.2.0".to_string(), false)
        );
    }

    #[test]
    fn checked_result_updates_tray_data_without_daemon_frame() {
        let mut data = TrayData::default();
        apply_checked_update(
            &mut data,
            &crate::update::UpdateInfo {
                available: true,
                current_version: "1.1.0".to_string(),
                latest_version: "1.2.0".to_string(),
                release_notes: String::new(),
                source_url: String::new(),
                is_npm: false,
                apply_mode: crate::update::UpdateApplyMode::Automatic,
                manual_command: String::new(),
            },
        );
        assert!(data.update_available);
        assert_eq!(data.update_latest, "1.2.0");
    }

    #[test]
    fn current_result_is_visible_and_check_remains_available() {
        let nodes = build_specs(
            false,
            Lang::En,
            &TrayData::default(),
            &[],
            false,
            false,
            &UpdateActionState::Current,
        );
        assert_eq!(
            item(&nodes, "st.update_action"),
            ("AskHuman is up to date", false)
        );
        assert_eq!(item(&nodes, "check_update"), ("Check for Updates", true));
    }

    #[test]
    fn found_result_remains_visible_after_menu_closes() {
        let nodes = build_specs(
            false,
            Lang::En,
            &TrayData {
                update_available: true,
                update_latest: "1.2.0".to_string(),
                ..Default::default()
            },
            &[],
            false,
            false,
            &UpdateActionState::Found("1.2.0".to_string()),
        );
        assert_eq!(
            item(&nodes, "st.update_action"),
            ("Update found: v1.2.0", false)
        );
        assert_eq!(item(&nodes, "check_update"), ("Check for Updates", true));
        let (action_id, _) =
            available_update_action(crate::update::apply_mode(), Lang::En, "1.2.0");
        assert!(item(&nodes, action_id).1);
    }

    #[test]
    fn applying_disables_concurrent_update_actions() {
        let data = TrayData {
            update_available: true,
            update_latest: "1.2.0".to_string(),
            ..Default::default()
        };
        let nodes = build_specs(
            false,
            Lang::En,
            &data,
            &[],
            false,
            false,
            &UpdateActionState::Applying,
        );
        assert_eq!(
            item(&nodes, "st.update_action"),
            ("Updating AskHuman…", false)
        );
        assert!(!item(&nodes, "check_update").1);
        let (action_id, _) =
            available_update_action(crate::update::apply_mode(), Lang::En, "1.2.0");
        assert!(!item(&nodes, action_id).1);
    }

    #[test]
    fn unsigned_windows_update_action_routes_to_manual_preparation() {
        assert_eq!(
            available_update_action(
                crate::update::UpdateApplyMode::ManualDirect,
                Lang::En,
                "1.2.0"
            ),
            (
                "prepare_update",
                "Prepare manual update to v1.2.0…".to_string()
            )
        );
        assert_eq!(
            available_update_action(crate::update::UpdateApplyMode::Automatic, Lang::En, "1.2.0").0,
            "apply_update"
        );
    }

    #[test]
    fn pending_update_explains_why_the_running_version_is_still_old() {
        let nodes = build_specs(
            true,
            Lang::En,
            &TrayData {
                running: true,
                version: "1.1.0".to_string(),
                active_requests: 2,
                draining: true,
                update_latest: "1.2.0".to_string(),
                pending: true,
                ..Default::default()
            },
            &[],
            false,
            false,
            &UpdateActionState::Idle,
        );

        assert_eq!(item(&nodes, "st.version"), ("Version 1.1.0", false));
        assert_eq!(
            item(&nodes, "st.update_pending"),
            (
                "↑ v1.2.0 installed — waiting for 2 in-flight request(s) before restarting daemon (won't interrupt them)",
                false,
            )
        );
        assert!(!has_item(&nodes, "st.draining"));
    }

    #[test]
    fn pending_update_reason_tracks_restart_stage() {
        let data = TrayData {
            update_latest: "1.2.0".to_string(),
            pending: true,
            ..Default::default()
        };
        assert_eq!(
            pending_update_text(true, &data, Lang::En),
            "↑ v1.2.0 installed — waiting for daemon to restart…"
        );
        assert_eq!(
            pending_update_text(false, &data, Lang::En),
            "↑ v1.2.0 installed — updated daemon starts on next use"
        );

        let restarting = TrayData {
            draining: true,
            ..data
        };
        assert_eq!(
            pending_update_text(true, &restarting, Lang::Zh),
            "↑ v1.2.0 已安装 — daemon 正在重启…"
        );
    }

    #[test]
    fn update_errors_are_single_line_and_bounded() {
        let compact = compact_update_error(
            "npm failed\nplease run npm i -g askhuman@latest because this explanation is deliberately long enough to truncate",
        );
        assert!(!compact.contains('\n'));
        assert!(compact.chars().count() <= 73);
        assert!(compact.ends_with('…'));

        let action = UpdateActionState::ApplyFailed(compact.clone());
        let text = update_action_text(&action, Lang::En).unwrap();
        assert_eq!(text, format!("⚠ Update failed: {compact}"));
    }

    #[test]
    fn multiple_integration_updates_are_clickable_and_compact() {
        let updates = vec!["cursor".to_string(), "codex".to_string()];
        let nodes = build_specs(
            false,
            Lang::En,
            &TrayData::default(),
            &updates,
            false,
            false,
            &UpdateActionState::Idle,
        );
        assert_eq!(
            item(&nodes, "integration_updates"),
            ("💡 2 Agent integrations need updates", true)
        );
    }

    #[test]
    fn single_integration_update_names_the_agent() {
        assert_eq!(
            integration_updates_text(&["codex".to_string()], Lang::Zh),
            "💡 Codex 集成需更新"
        );
    }

    #[test]
    fn pending_reply_icon_takes_priority_over_integration_attention() {
        assert_eq!(icon_source(false, 0, true), icon_bytes::STOPPED_ATTENTION);
        assert_eq!(icon_source(true, 0, true), icon_bytes::IDLE_ATTENTION);
        assert_eq!(icon_source(true, 1, true), icon_bytes::ACTIVE);
        assert_eq!(icon_source(true, 0, false), icon_bytes::IDLE);
    }

    #[test]
    fn windows_tray_compaction_trims_and_centers_rectangular_artwork() {
        let mut rgba = vec![0; 8 * 6 * 4];
        for y in 2..4 {
            for x in 2..6 {
                rgba[(y * 8 + x) * 4 + 3] = 255;
            }
        }

        let compact = compact_windows_tray_icon(Image::new_owned(rgba, 8, 6));
        assert_eq!((compact.width(), compact.height()), (6, 6));

        let alpha = |x: usize, y: usize| compact.rgba()[(y * 6 + x) * 4 + 3];
        assert_eq!(alpha(0, 2), 0);
        assert_eq!(alpha(1, 2), 255);
        assert_eq!(alpha(4, 3), 255);
        assert_eq!(alpha(5, 3), 0);
    }

    #[test]
    fn windows_tray_compaction_enlarges_the_shipped_artwork_canvas_share() {
        let idle = Image::from_bytes(icon_bytes::IDLE).unwrap();
        assert_eq!((idle.width(), idle.height()), (48, 36));

        let compact = compact_windows_tray_icon(idle);
        assert_eq!((compact.width(), compact.height()), (38, 38));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn non_windows_tray_decode_preserves_the_designed_canvas() {
        let idle = decode_icon(icon_bytes::IDLE).unwrap();
        assert_eq!((idle.width(), idle.height()), (48, 36));
    }
}
