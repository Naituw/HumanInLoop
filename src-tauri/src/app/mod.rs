//! Tauri 运行时：创建窗口、并行启动 Channel、汇集结果并退出。

pub mod confirm_coordinator;
pub mod coordinator;
pub mod gui_host;
pub mod terminal_gate;
pub mod tray_menu;

use crate::channels::dingding::DingTalkChannel;
use crate::channels::feishu::FeishuChannel;
use crate::channels::popup::PopupChannel;
use crate::channels::slack::SlackChannel;
use crate::channels::telegram::TelegramChannel;
use crate::channels::Channel;
use crate::cli::{image_writer, output};
use crate::config::{AppConfig, ThemeMode, WindowEffect};
use crate::dingtalk::client::DingTalkClient;
use crate::feishu::client::FeishuClient;
use crate::i18n::{self, Lang};
use crate::models::{AskRequest, ChannelAction, ChannelResult, InteractionRequest, QuestionAnswer};
use crate::slack::client::SlackClient;
use crate::telegram::TelegramClient;
use coordinator::Coordinator;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
#[cfg(target_os = "macos")]
use tauri::window::{Effect, EffectState, EffectsBuilder};
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// 运行时只读状态：供 popup_init 拉取请求内容与主题。
pub struct AppState {
    pub interaction: InteractionRequest,
    /// Native edit intent used only by the local permission popup.
    pub popup_edit: Option<Box<crate::permission_diff::PermissionEditIntent>>,
    pub config: AppConfig,
    /// 来源名（弹窗标题「Question from {source}」）。Daemon 模式由调用方上送（A11）；
    /// 设置 / 非 Daemon 回退路径取本进程环境。
    pub source: String,
    /// 当前项目 key（回复历史归类 / 历史窗口默认过滤）。Daemon 模式由调用方上送；
    /// 单进程 / 独立窗口在本进程计算（向上找 .git 根、回退 cwd）。
    pub project: String,
    /// 发起本次提问的 agent 家族（claude/codex/cursor/grok），仅弹窗（Daemon 上送）有值；其它窗口为 None。
    pub agent_kind: Option<String>,
    /// Native Agent session used by reply-history filtering. This is independent from whether the
    /// session is currently tracked by AgentRegistry.
    pub agent_session_id: Option<String>,
    /// MCP process fallback used only when no native Agent session is available.
    pub mcp_instance_id: Option<String>,
    /// 发起本次提问的 agent 进程 pid，仅弹窗（Daemon 上送）有值；用于「聚焦终端」与终端可激活性判断。
    pub agent_pid: Option<u32>,
    /// 已严格匹配到 AgentRegistry 活动记录的会话 ID。仅用于弹窗打开并定位 Agent 状态窗口。
    pub agent_console_session_id: Option<String>,
    /// 提问创建时刻（epoch 毫秒）：弹窗相对时间的锚点。冷/单进程路径取弹窗构造时刻；GUI helper 取 `Show`
    /// 透传的创建时刻。非弹窗窗口（设置/历史/Agents/GuiHost）不使用，置 0。
    pub created_at_ms: u64,
}

impl AppState {
    fn ask_request(&self) -> &AskRequest {
        self.interaction
            .ask()
            .expect("non-daemon popup state must carry an ask request")
    }
}

#[derive(Clone, Copy)]
enum View {
    Popup,
    Settings,
    /// 独立历史窗口；`all` 为 true 时默认展示全部项目。
    History {
        all: bool,
    },
    /// 独立项目待办窗口（`AskHuman --todos`）；预选项目取自 `AppState.project`。
    Todos,
    /// Agent 生命周期状态窗口（实验性功能，spec D13）：订阅 daemon 推送，动态更新。
    Agents,
    /// 统一 GUI 宿主（菜单栏托盘 + 各窗口单实例，spec D2）：无初始窗口，常驻事件循环。
    GuiHost,
}

/// GUI Helper 模式下，弹窗 ↔ Daemon 的 IPC 接线（由 `run_gui_helper` 建好后传入 `launch`）。
pub struct PopupIpc {
    /// 向 Daemon 发送 `answer` 等消息（写任务已在 `run_gui_helper` 中起好）。
    pub gui_tx: tokio::sync::mpsc::UnboundedSender<crate::ipc::ClientMsg>,
    /// Daemon 分配的 request_id（回带在 `answer` 中）。预热（warm）模式领用前为空，收到 `Show` 时填入。
    pub request_id: String,
    /// 读取 Daemon → GUI 的消息流（cancel / 连接断开 / 预热模式下的首条 `Show` 领用）。
    pub reader: std::pin::Pin<Box<dyn tokio::io::AsyncBufRead + Send>>,
    /// 方案6 预热模式：true 表示本进程是「热弹窗」——建窗后隐藏待命、不带请求，由首条 `Show` 领用上屏。
    pub warm: bool,
}

/// 方案6 预热弹窗的「领用槽」：热进程建窗挂载后停在待命态（`show=None`）；daemon 发来 `Show` 即填入，
/// 前端经 `popup_init` 读到后渲染、绘制完成才 `show()`。仅预热弹窗进程 manage 本状态。
pub struct WarmPopup {
    pub show: std::sync::Mutex<Option<crate::ipc::ShowPayload>>,
    pub finalized: AtomicBool,
}

/// 弹窗作答 → Daemon 的桥：把前端 `submit_popup` / `cancel_popup` 转成 IPC `answer` 发回 Daemon。
/// 仅 GUI Helper 模式存在；单进程（非 unix 回退）路径用 `Coordinator`。
pub struct GuiBridge {
    tx: tokio::sync::mpsc::UnboundedSender<crate::ipc::ClientMsg>,
    /// Daemon 分配的 request_id（回带在 `answer` 中）。预热弹窗领用前为空，收到 `Show` 时由 reader 循环填入，
    /// 故用内部可变。
    request_id: std::sync::Mutex<String>,
    /// 仅投递一次（发送/取消互斥，去重）。
    done: AtomicBool,
    /// Content/native window readiness is reported exactly once.
    ready_sent: AtomicBool,
    /// Daemon presentation authorization is applied exactly once.
    presented: AtomicBool,
    app: tauri::AppHandle,
}

impl GuiBridge {
    /// 预热弹窗领用时回填 request_id（仅一次）。
    pub fn set_request_id(&self, id: String) {
        if let Ok(mut g) = self.request_id.lock() {
            *g = id;
        }
    }

    fn terminal(&self, message: crate::ipc::ClientMsg) {
        if self.done.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.tx.send(message);
        // 即时关窗，视觉上与单进程一致（进程随后由 Daemon 关闭连接 / 安全网驱动退出）。
        if let Some(w) = self.app.get_webview_window("popup") {
            let _ = w.close();
        }
        // 安全网：正常情况下 Daemon 收到答复后关闭连接 → reader EOF → 退出；
        // 万一 Daemon 无响应，到时也主动退出，避免弹窗进程悬挂。
        let app = self.app.clone();
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            app.exit(0);
        });
    }

    pub fn dismiss_from_daemon(&self) {
        self.done.store(true, Ordering::SeqCst);
        if let Some(window) = self.app.get_webview_window("popup") {
            let _ = window.close();
        } else {
            self.send_popup_dismissed();
        }
    }

    pub fn send_popup_ready(&self, window_number: Option<i64>) {
        if self.ready_sent.swap(true, Ordering::SeqCst) {
            return;
        }
        let _ = self.tx.send(crate::ipc::ClientMsg::PopupReady {
            request_id: self.request_id(),
            window_number,
        });
    }

    pub fn send_popup_focused(&self) {
        if !self.ready_sent.load(Ordering::SeqCst) || self.is_done() {
            return;
        }
        let _ = self.tx.send(crate::ipc::ClientMsg::PopupFocused {
            request_id: self.request_id(),
        });
    }

    pub fn send_popup_dismissed(&self) {
        if !self.ready_sent.load(Ordering::SeqCst) {
            return;
        }
        let _ = self.tx.send(crate::ipc::ClientMsg::PopupDismissed {
            request_id: self.request_id(),
        });
    }

    fn begin_presentation(&self) -> bool {
        !self.presented.swap(true, Ordering::SeqCst)
    }

    /// 提交作答。
    pub fn send_answer(&self, answers: Vec<QuestionAnswer>) {
        self.terminal(crate::ipc::ClientMsg::Answer {
            request_id: self.request_id(),
            action: ChannelAction::Send,
            answers,
        });
    }

    /// 取消（关窗 / Cmd+Q）。
    pub fn send_cancel(&self) {
        self.terminal(crate::ipc::ClientMsg::Answer {
            request_id: self.request_id(),
            action: ChannelAction::Cancel,
            answers: Vec::new(),
        });
    }

    pub fn send_confirm_answer(&self, choice_index: usize, comment: Option<String>) {
        self.terminal(crate::ipc::ClientMsg::ConfirmAnswer {
            request_id: self.request_id(),
            choice_index,
            comment,
        });
    }

    pub fn send_confirm_ready(&self) {
        if !self.done.load(Ordering::SeqCst) {
            let _ = self.tx.send(crate::ipc::ClientMsg::ConfirmReady {
                request_id: self.request_id(),
            });
        }
    }

    fn request_id(&self) -> String {
        self.request_id
            .lock()
            .map(|id| id.clone())
            .unwrap_or_default()
    }

    /// 是否已进入收尾（已提交/取消）：关窗事件据此放行，避免拦截导致无法真正关窗。
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

fn cascade_popup_position(win: &tauri::WebviewWindow, cascade_index: u32) {
    if cascade_index == 0 {
        return;
    }
    let (Ok(position), Ok(size), Ok(scale), Ok(Some(monitor))) = (
        win.outer_position(),
        win.outer_size(),
        win.scale_factor(),
        win.current_monitor(),
    ) else {
        return;
    };
    let step = (24.0 * scale).round().max(1.0) as i32;
    let monitor_position = monitor.position();
    let monitor_size = monitor.size();
    let max_x = monitor_position
        .x
        .saturating_add(monitor_size.width.saturating_sub(size.width) as i32);
    let max_y = monitor_position
        .y
        .saturating_add(monitor_size.height.saturating_sub(size.height) as i32);
    let slots_x = max_x.saturating_sub(position.x).max(0) / step;
    let slots_y = max_y.saturating_sub(position.y).max(0) / step;
    let slots = slots_x.min(slots_y);
    let slot = if slots > 0 {
        ((cascade_index.saturating_sub(1) % slots as u32) + 1) as i32
    } else {
        0
    };
    let _ = win.set_position(tauri::PhysicalPosition::new(
        position.x.saturating_add(step.saturating_mul(slot)),
        position.y.saturating_add(step.saturating_mul(slot)),
    ));
}

/// Present a fully rendered helper window according to the daemon-owned focus decision.
/// Foreground uses the regular Tauri activation path; background cascade must not activate NSApp.
#[cfg(unix)]
pub(crate) fn finalize_popup_show(
    app: &tauri::AppHandle,
    presentation: crate::ipc::PopupPresentation,
) {
    use tauri::Manager;
    if let Some(bridge) = app.try_state::<GuiBridge>() {
        if !bridge.begin_presentation() {
            return;
        }
    }
    if let Some(warm) = app.try_state::<WarmPopup>() {
        if warm.finalized.swap(true, Ordering::SeqCst) {
            return;
        }
    }
    let Some(win) = app.get_webview_window("popup") else {
        return;
    };
    // 预热期间用户若改过尺寸/置顶，这里按最新 config 兜底（主题由构建 + ConfigChanged 已同步）。
    let config = AppConfig::load_without_secrets();
    let _ = win.set_size(tauri::LogicalSize::new(
        config.channels.popup.width,
        config.channels.popup.height,
    ));
    let _ = win.set_always_on_top(config.general.always_on_top);
    // Apply the latest native appearance in case the theme changed while prewarmed.
    crate::commands::apply_theme_to_windows(app, &crate::commands::theme_str(config.general.theme));
    #[cfg(target_os = "macos")]
    {
        // 方案6：领用上屏 → 切回 Regular，让弹窗入坞（待命期为 accessory，不占 Dock/Cmd-Tab）。
        let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
        // 待命期（accessory）设的 applicationIconImage 在切回 Regular 后会被 AppKit 用默认图标覆盖
        // （裸二进制 → 通用命令行图标），故此刻在 Regular 下重设一次内置图标，确保 Dock 显示正确图标。
        crate::macos_dock_icon::set_dock_icon();
        if let Ok(ns) = win.ns_window() {
            crate::macos_window_anim::set_appear_animation(
                ns,
                config.general.appear_animation.ns_animation_behavior(),
            );
        }
        // Reapply the complete current material before showing a prewarmed window.
        set_runtime_window_effect(&win, config.general.window_effect);
        let count = app
            .try_state::<WarmPopup>()
            .and_then(|w| {
                w.show.lock().ok().and_then(|g| {
                    g.as_ref()
                        .and_then(|s| s.interaction.ask())
                        .map(|request| request.questions.len())
                })
            })
            .or_else(|| {
                app.try_state::<AppState>().and_then(|state| {
                    state
                        .interaction
                        .ask()
                        .map(|request| request.questions.len())
                })
            })
            .unwrap_or(0);
        crate::macos_dock_icon::announce_questions(count);
    }
    match presentation {
        crate::ipc::PopupPresentation::Foreground => {
            let _ = win.show();
            let _ = win.set_focus();
        }
        crate::ipc::PopupPresentation::BackgroundCascade {
            cascade_index,
            behind_window_number,
        } => {
            #[cfg(target_os = "macos")]
            if let Ok(ns_window) = win.ns_window() {
                crate::macos_window_order::cascade(ns_window, cascade_index);
                crate::macos_window_order::show_behind(ns_window, behind_window_number);
            }
            #[cfg(not(target_os = "macos"))]
            {
                let _ = behind_window_number;
                cascade_popup_position(&win, cascade_index);
                let _ = win.show();
            }
        }
    }
    crate::perf::mark_env("gui.win_show");
    crate::sound::play(&config.general.popup_sound);
}

/// 无任何可用通信 Channel 时的退出码（供下游据此降级）。
pub const EXIT_NO_CHANNEL: i32 = 3;

/// 提问模式入口：按 Channel 可用性分流到 GUI 弹窗或 headless 消息渠道。
///
/// 决策（在创建任何窗口前）：
/// - 需要弹窗且 GUI 可用 → GUI 路径（弹窗 + 可选会话型渠道抢答）；
/// - 否则若存在可用会话型渠道（Telegram/钉钉）→ headless 路径（不进 Tauri）；
/// - 都不可用 → stderr 报原因 + 退出码 `EXIT_NO_CHANNEL`。
pub fn run_ask(request: AskRequest, config: AppConfig) -> ! {
    let lang = Lang::resolve(&config.general.language);
    let messaging_active = has_active_messaging(&config);
    let popup_wanted = config.channels.popup.enabled;
    let gui = gui_available(lang);

    if popup_wanted && gui.is_ok() {
        run_gui_ask(request, config, messaging_active);
    } else if messaging_active {
        if popup_wanted {
            if let Err(reason) = &gui {
                stderr_redirect::eprintln_real(
                    &i18n::tr(lang, "app.popupUnavailableFellBack").replace("{reason}", reason),
                );
            }
        }
        run_headless(request, config);
    } else {
        let reason = match (popup_wanted, &gui) {
            (true, Err(r)) => {
                i18n::tr(lang, "app.popupUnavailableNoChannel").replace("{reason}", r)
            }
            (false, _) => i18n::tr(lang, "app.popupDisabledNoChannel").to_string(),
            (true, Ok(())) => unreachable!(),
        };
        stderr_redirect::eprintln_real(&format!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "app.noChannel").replace("{reason}", &reason)
        ));
        std::process::exit(EXIT_NO_CHANNEL);
    }
}

/// Telegram 是否已配置且可用（构造 client 成功即视为可用）。
pub(crate) fn is_telegram_active(config: &AppConfig) -> bool {
    let tg = &config.channels.telegram;
    tg.enabled
        && TelegramClient::new(
            tg.bot_token.clone(),
            tg.chat_id.clone(),
            tg.api_base_url.clone(),
        )
        .is_ok()
}

/// 钉钉是否已配置且可用（构造 client 成功——即三项非空——即视为可用）。
pub(crate) fn is_dingding_active(config: &AppConfig) -> bool {
    let dd = &config.channels.dingding;
    dd.enabled && DingTalkClient::new(dd).is_ok()
}

/// 飞书是否已配置且可用（构造 client 成功且 open_id 非空——即四项齐备——即视为可用）。
pub(crate) fn is_feishu_active(config: &AppConfig) -> bool {
    let fs = &config.channels.feishu;
    fs.enabled && !fs.open_id.trim().is_empty() && FeishuClient::new(fs).is_ok()
}

/// Slack 是否已配置且可用（构造 client 成功——双 token 齐备——且 user_id 非空即视为可用）。
pub(crate) fn is_slack_active(config: &AppConfig) -> bool {
    let sl = &config.channels.slack;
    sl.enabled && !sl.user_id.trim().is_empty() && SlackClient::new(sl).is_ok()
}

/// 是否存在任一可用的会话型消息渠道。
fn has_active_messaging(config: &AppConfig) -> bool {
    is_telegram_active(config)
        || is_dingding_active(config)
        || is_feishu_active(config)
        || is_slack_active(config)
}

/// 收集全部可用的会话型渠道外层（供 GUI 路径注册并行抢答）。
fn active_messaging_channels(config: &AppConfig) -> Vec<Arc<dyn Channel>> {
    let mut channels: Vec<Arc<dyn Channel>> = Vec::new();
    if is_telegram_active(config) {
        channels.push(Arc::new(TelegramChannel::new(
            config.channels.telegram.clone(),
        )));
    }
    if is_dingding_active(config) {
        channels.push(Arc::new(DingTalkChannel::new(
            config.channels.dingding.clone(),
        )));
    }
    if is_feishu_active(config) {
        channels.push(Arc::new(FeishuChannel::new(config.channels.feishu.clone())));
    }
    if is_slack_active(config) {
        channels.push(Arc::new(SlackChannel::new(config.channels.slack.clone())));
    }
    channels
}

/// GUI 弹窗路径；若 Tauri 构建失败（GUI 不可用），按消息渠道是否可用兜底。
fn run_gui_ask(request: AskRequest, config: AppConfig, messaging_active: bool) -> ! {
    let lang = Lang::resolve(&config.general.language);
    let caller = crate::cli::caller_context();
    let state = AppState {
        interaction: InteractionRequest::Ask(request.clone()),
        popup_edit: None,
        config: config.clone(),
        source: crate::models::source_name(),
        project: crate::project::detect(),
        agent_kind: caller.agent_kind,
        agent_session_id: caller.agent_session_id,
        mcp_instance_id: caller.mcp_instance_id,
        agent_pid: None,
        agent_console_session_id: None,
        // 单进程弹窗无 daemon：以构造时刻为提问时间锚点。
        created_at_ms: crate::perf::now_ms() as u64,
    };
    match launch(state, View::Popup, None) {
        Ok(()) => std::process::exit(0), // 成功路径已在 launch 内退出，此处不可达
        Err(e) => {
            if messaging_active {
                stderr_redirect::eprintln_real(
                    &i18n::tr(lang, "app.popupStartFailedFellBack").replace("{e}", &e.to_string()),
                );
                run_headless(request, config);
            } else {
                stderr_redirect::eprintln_real(&format!(
                    "{}{}",
                    i18n::err_prefix(lang),
                    i18n::tr(lang, "app.popupStartFailedNoChannel").replace("{e}", &e.to_string())
                ));
                std::process::exit(EXIT_NO_CHANNEL);
            }
        }
    }
}

/// headless 路径：不进入 Tauri 事件循环，用 tokio 并行跑全部可用会话型渠道。
///
/// 直接驱动各渠道会话并 `await` 全部结束：任一渠道完成回复即 `submit` → `process::exit`；
/// 全部会话结束仍无结果 → 报错并以 `EXIT_NO_CHANNEL` 退出（避免静默挂起）。
fn run_headless(request: AskRequest, config: AppConfig) -> ! {
    use crate::channels::conversation::{run_conversation, MessagingChannel};

    let lang = Lang::resolve(&config.general.language);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            stderr_redirect::eprintln_real(&format!(
                "{}{}",
                i18n::err_prefix(lang),
                i18n::tr(lang, "app.runtimeCreateFailed").replace("{e}", &e.to_string())
            ));
            std::process::exit(1);
        }
    };

    // 并行消息渠道数（用于抢答收尾计算落败端数）+ 共享抢答信号。
    let messaging_count = is_telegram_active(&config) as usize
        + is_dingding_active(&config) as usize
        + is_feishu_active(&config) as usize
        + is_slack_active(&config) as usize;
    let preempt = Arc::new(crate::channels::Preemption::new());
    let project = crate::project::detect();
    let source = crate::models::source_name();
    let origin = crate::channels::ConversationOrigin::new(&source, None, &project);
    let coordinator = Coordinator::new_headless(
        request.clone(),
        preempt.clone(),
        messaging_count,
        project,
        source,
    );

    rt.block_on(async move {
        let mut handles = Vec::new();

        if is_telegram_active(&config) {
            use crate::channels::telegram::TelegramSession;
            use crate::telegram::router::TgRouter;
            let cfg = config.channels.telegram.clone();
            let req = request.clone();
            let sink = coordinator.clone();
            let preempt = preempt.clone();
            let origin = origin.clone();
            handles.push(tokio::spawn(async move {
                // 单进程：每进程起一个仅挂本会话的 Router（统一走 Router 路径，单一 offset）。
                let router = match TgRouter::connect(&cfg).await {
                    Ok(r) => r,
                    Err(e) => {
                        stderr_redirect::eprintln_real(&format!(
                            "{}{}",
                            i18n::warn_prefix(lang),
                            i18n::tr(lang, "app.telegramInvalid").replace("{e}", &e)
                        ));
                        return;
                    }
                };
                let events = router.register();
                let mut session = TelegramSession::new(cfg, events);
                if let Err(e) = session.open().await {
                    stderr_redirect::eprintln_real(&format!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "app.telegramInvalid").replace("{e}", &e.to_string())
                    ));
                    return;
                }
                run_conversation(&mut session, &req, &origin, preempt, sink).await;
            }));
        }

        if is_dingding_active(&config) {
            use crate::channels::dingding::DingTalkSession;
            use crate::dingtalk::router::DdRouter;
            let cfg = config.channels.dingding.clone();
            let req = request.clone();
            let sink = coordinator.clone();
            let preempt = preempt.clone();
            let origin = origin.clone();
            handles.push(tokio::spawn(async move {
                // 单进程：每进程起一个仅挂本会话的 Router（统一走 Router 路径）。
                let router =
                    match DdRouter::connect(cfg.client_id.trim(), cfg.client_secret.trim()).await {
                        Ok(r) => r,
                        Err(e) => {
                            stderr_redirect::eprintln_real(&format!(
                                "{}{}",
                                i18n::warn_prefix(lang),
                                i18n::tr(lang, "app.dingtalkInvalid").replace("{e}", &e)
                            ));
                            return;
                        }
                    };
                let events = router.register();
                let mut session = DingTalkSession::new(cfg, events);
                if let Err(e) = session.open().await {
                    stderr_redirect::eprintln_real(&format!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "app.dingtalkInvalid").replace("{e}", &e.to_string())
                    ));
                    return;
                }
                run_conversation(&mut session, &req, &origin, preempt, sink).await;
            }));
        }

        if is_feishu_active(&config) {
            use crate::channels::feishu::FeishuSession;
            use crate::feishu::router::FsRouter;
            let cfg = config.channels.feishu.clone();
            let req = request.clone();
            let sink = coordinator.clone();
            let preempt = preempt.clone();
            let origin = origin.clone();
            handles.push(tokio::spawn(async move {
                // 单进程：每进程起一个仅挂本会话的 Router（统一走 Router 路径）。
                let router = match FsRouter::connect(&cfg).await {
                    Ok(r) => r,
                    Err(e) => {
                        stderr_redirect::eprintln_real(&format!(
                            "{}{}",
                            i18n::warn_prefix(lang),
                            i18n::tr(lang, "app.feishuInvalid").replace("{e}", &e)
                        ));
                        return;
                    }
                };
                let events = router.register();
                let mut session = FeishuSession::new(cfg, events);
                if let Err(e) = session.open().await {
                    stderr_redirect::eprintln_real(&format!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "app.feishuInvalid").replace("{e}", &e)
                    ));
                    return;
                }
                run_conversation(&mut session, &req, &origin, preempt, sink).await;
            }));
        }

        if is_slack_active(&config) {
            use crate::channels::slack::SlackSession;
            use crate::slack::router::SlRouter;
            let cfg = config.channels.slack.clone();
            let req = request.clone();
            let sink = coordinator.clone();
            let preempt = preempt.clone();
            let origin = origin.clone();
            handles.push(tokio::spawn(async move {
                // 单进程：每进程起一个仅挂本会话的 Router（统一走 Router 路径，独占一条 Socket Mode 连接）。
                let router = match SlRouter::connect(&cfg).await {
                    Ok(r) => r,
                    Err(e) => {
                        stderr_redirect::eprintln_real(&format!(
                            "{}{}",
                            i18n::warn_prefix(lang),
                            i18n::tr(lang, "app.slackInvalid").replace("{e}", &e)
                        ));
                        return;
                    }
                };
                let events = router.register();
                let mut session = SlackSession::new(cfg, events);
                if let Err(e) = session.open().await {
                    stderr_redirect::eprintln_real(&format!(
                        "{}{}",
                        i18n::warn_prefix(lang),
                        i18n::tr(lang, "app.slackInvalid").replace("{e}", &e)
                    ));
                    return;
                }
                run_conversation(&mut session, &req, &origin, preempt, sink).await;
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        // 全部会话结束：若已有结果则输出并退出（不返回）；否则返回交由下方兜底报错。
        coordinator.finish();
    });

    // 正常情况下用户完成回复 → submit → 进程已退出；走到此处说明全部会话结束仍未获结果。
    stderr_redirect::eprintln_real(&format!(
        "{}{}",
        i18n::err_prefix(lang),
        i18n::tr(lang, "app.sessionEndedNoResult")
    ));
    std::process::exit(EXIT_NO_CHANNEL);
}

/// 设置模式：创建设置窗口。
pub fn run_settings(config: AppConfig) -> ! {
    let lang = Lang::resolve(&config.general.language);
    let state = AppState {
        interaction: InteractionRequest::Ask(AskRequest::new(
            crate::models::MessagePrompt::default(),
            Vec::new(),
            false,
        )),
        popup_edit: None,
        config,
        source: crate::models::source_name(),
        project: crate::project::detect(),
        agent_kind: None,
        agent_session_id: None,
        mcp_instance_id: None,
        agent_pid: None,
        agent_console_session_id: None,
        created_at_ms: 0,
    };
    if let Err(e) = launch(state, View::Settings, None) {
        stderr_redirect::eprintln_real(&format!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "app.settingsLaunchFailed").replace("{e}", &e.to_string())
        ));
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// 历史模式：创建独立历史窗口（独立 GUI 进程，不经 Daemon；与 `--settings` 同机制）。
/// `all` 为 true 时默认展示全部项目，否则默认 `project`（向上找 .git 根、回退 cwd）。
pub fn run_history(project: String, all: bool, config: AppConfig) -> ! {
    let lang = Lang::resolve(&config.general.language);
    let state = AppState {
        interaction: InteractionRequest::Ask(AskRequest::new(
            crate::models::MessagePrompt::default(),
            Vec::new(),
            false,
        )),
        popup_edit: None,
        config,
        source: crate::models::source_name(),
        project,
        agent_kind: None,
        agent_session_id: None,
        mcp_instance_id: None,
        agent_pid: None,
        agent_console_session_id: None,
        created_at_ms: 0,
    };
    if let Err(e) = launch(state, View::History { all }, None) {
        stderr_redirect::eprintln_real(&format!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "app.historyLaunchFailed").replace("{e}", &e.to_string())
        ));
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// 待办窗口模式：独立进程建窗（gui-host 不可用时的兜底；与 `--settings` / `--history` 同机制）。
/// `project` 为预选项目 key（通常是 CLI cwd 的 git 根），写入 `AppState.project`。
pub fn run_todos(project: String, config: AppConfig) -> ! {
    let lang = Lang::resolve(&config.general.language);
    let state = AppState {
        interaction: InteractionRequest::Ask(AskRequest::new(
            crate::models::MessagePrompt::default(),
            Vec::new(),
            false,
        )),
        popup_edit: None,
        config,
        source: crate::models::source_name(),
        project,
        agent_kind: None,
        agent_session_id: None,
        mcp_instance_id: None,
        agent_pid: None,
        agent_console_session_id: None,
        created_at_ms: 0,
    };
    if let Err(e) = launch(state, View::Todos, None) {
        stderr_redirect::eprintln_real(&format!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "app.todosLaunchFailed").replace("{e}", &e.to_string())
        ));
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// Agent 状态窗口入口（`AskHuman agents status`，实验性功能 spec D13）：
/// 创建窗口 + 订阅 daemon 推送，动态展示工作中 / 空闲 / 已结束的 agent。
pub fn run_agents(config: AppConfig) -> ! {
    let lang = Lang::resolve(&config.general.language);
    let state = AppState {
        interaction: InteractionRequest::Ask(AskRequest::new(
            crate::models::MessagePrompt::default(),
            Vec::new(),
            false,
        )),
        popup_edit: None,
        config,
        source: crate::models::source_name(),
        project: crate::project::detect(),
        agent_kind: None,
        agent_session_id: None,
        mcp_instance_id: None,
        agent_pid: None,
        agent_console_session_id: None,
        created_at_ms: 0,
    };
    if let Err(e) = launch(state, View::Agents, None) {
        stderr_redirect::eprintln_real(&format!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "app.agentsLaunchFailed").replace("{e}", &e.to_string())
        ));
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// 统一 GUI 宿主入口（`AskHuman --gui-host`，spec D2）：单实例托盘 + 设置/历史/Agent 窗口宿主。
///
/// 抢宿主单实例锁失败（已有宿主）即直接退出；成功则进入 Tauri 事件循环常驻，
/// 经自有 IPC 接收开窗请求、订阅 daemon 状态驱动托盘、监听配置热更新。
pub fn run_gui_host(config: AppConfig) -> ! {
    if !gui_host::acquire_singleton() {
        // 已有宿主在跑（或锁被占）：本进程多余，直接退出。
        std::process::exit(0);
    }
    let state = AppState {
        interaction: InteractionRequest::Ask(AskRequest::new(
            crate::models::MessagePrompt::default(),
            Vec::new(),
            false,
        )),
        popup_edit: None,
        config,
        source: crate::models::source_name(),
        project: crate::project::detect(),
        agent_kind: None,
        agent_session_id: None,
        mcp_instance_id: None,
        agent_pid: None,
        agent_console_session_id: None,
        created_at_ms: 0,
    };
    if let Err(e) = launch(state, View::GuiHost, None) {
        stderr_redirect::eprintln_real(&format!("askhuman gui-host failed: {}", e));
        std::process::exit(1);
    }
    std::process::exit(0);
}

/// GUI Helper 模式入口（`AskHuman --popup --endpoint <sock> --token <tok>`，由 Daemon 拉起）。
///
/// 流程：连 Daemon → 出示一次性 token → 收 `show` → 本进程主线程跑 Tauri 弹窗；
/// 用户作答 / 取消经 IPC `answer` 回 Daemon；收到 `cancel` 或连接断开即退出。
pub fn run_gui_helper(_endpoint: String, token: String, warm: bool) -> ! {
    use crate::ipc::{self, transport, ClientMsg, ServerMsg};
    use tokio::io::BufReader;

    crate::perf::mark_env("gui.start");

    // 方案6 预热模式：连接 + 发 `GuiWarmReady`，立即建窗挂载、隐藏待命；首条 `Show` 在 reader 循环里领用。
    if warm {
        let connected = tauri::async_runtime::block_on(async move {
            let stream = transport::connect().await?;
            let (r, mut w) = stream.into_split();
            ipc::write_msg(&mut w, &ClientMsg::GuiWarmReady).await?;
            Ok::<_, std::io::Error>((BufReader::new(r), w))
        });
        let (reader, writer) = match connected {
            Ok(v) => v,
            Err(e) => {
                stderr_redirect::eprintln_real(&format!("askhuman warm popup helper: {}", e));
                std::process::exit(3);
            }
        };
        let (gui_tx, mut gui_rx) = tokio::sync::mpsc::unbounded_channel::<ClientMsg>();
        tauri::async_runtime::spawn(async move {
            let mut writer = writer;
            while let Some(msg) = gui_rx.recv().await {
                if ipc::write_msg(&mut writer, &msg).await.is_err() {
                    break;
                }
            }
        });
        // 待命态：无请求；source/project/agent 等领用时由 `Show` 注入（见 setup 的 reader 循环）。
        let state = AppState {
            interaction: InteractionRequest::Ask(AskRequest::new(
                crate::models::MessagePrompt::default(),
                Vec::new(),
                false,
            )),
            popup_edit: None,
            config: AppConfig::load_without_secrets(),
            source: String::new(),
            project: String::new(),
            agent_kind: None,
            agent_session_id: None,
            mcp_instance_id: None,
            agent_pid: None,
            agent_console_session_id: None,
            // 待命态：领用时由 `Show` 注入真正的创建时刻（popup_init 读 WarmPopup.show）。
            created_at_ms: 0,
        };
        let popup_ipc = PopupIpc {
            gui_tx,
            request_id: String::new(),
            reader: Box::pin(reader),
            warm: true,
        };
        if let Err(e) = launch(state, View::Popup, Some(popup_ipc)) {
            stderr_redirect::eprintln_real(&format!("askhuman warm popup helper failed: {}", e));
            std::process::exit(3);
        }
        std::process::exit(0);
    }

    // 连接 + 握手 + 读 show（在 Tauri 全局运行时上完成，确保后续读写任务同一 reactor）。
    let connected = tauri::async_runtime::block_on(async move {
        let stream = transport::connect().await?;
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        ipc::write_msg(&mut w, &ClientMsg::GuiHello { token }).await?;
        loop {
            match ipc::read_msg::<_, ServerMsg>(&mut reader).await? {
                Some(ServerMsg::Show(show)) => return Ok::<_, std::io::Error>((show, reader, w)),
                Some(_) => continue,
                None => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "daemon closed before show",
                    ))
                }
            }
        }
    });

    let (show, reader, writer) = match connected {
        Ok(v) => v,
        Err(e) => {
            stderr_redirect::eprintln_real(&format!("askhuman popup helper: {}", e));
            std::process::exit(3);
        }
    };
    crate::perf::mark_env("gui.show_recv");

    // 写任务：把 answer / 取消等消息串行写回 Daemon。
    let (gui_tx, mut gui_rx) = tokio::sync::mpsc::unbounded_channel::<ClientMsg>();
    tauri::async_runtime::spawn(async move {
        let mut writer = writer;
        while let Some(msg) = gui_rx.recv().await {
            if ipc::write_msg(&mut writer, &msg).await.is_err() {
                break;
            }
        }
    });

    let request_id = show.request_id.clone();
    let state = AppState {
        interaction: show.interaction,
        popup_edit: show.popup_edit,
        // The popup helper never connects to IM (the daemon does); it only needs general/theme/
        // popup-size config. Skip keychain via load_without_secrets().
        config: AppConfig::load_without_secrets(),
        source: show.source,
        project: show.project,
        agent_kind: show.agent_kind,
        agent_session_id: show.agent_session_id,
        mcp_instance_id: show.mcp_instance_id,
        agent_pid: show.agent_pid,
        agent_console_session_id: show.agent_console_session_id,
        created_at_ms: show.created_at_ms,
    };
    let popup_ipc = PopupIpc {
        gui_tx,
        request_id,
        reader: Box::pin(reader),
        warm: false,
    };
    if let Err(e) = launch(state, View::Popup, Some(popup_ipc)) {
        stderr_redirect::eprintln_real(&format!("askhuman popup helper failed: {}", e));
        std::process::exit(3);
    }
    std::process::exit(0);
}

/// 统一启动入口：`generate_context!` 每个二进制只能展开一次，故所有窗口共用此路径。
/// 成功路径在内部进入事件循环并退出进程（不返回）；构建失败返回 `Err` 供调用方兜底。
fn launch(state: AppState, view: View, popup_ipc: Option<PopupIpc>) -> tauri::Result<()> {
    let theme = window_theme(&state.config);
    let lang = Lang::resolve(&state.config.general.language);
    let window_bg = background_for(resolved_theme(&state.config));
    let popup_w = state.config.channels.popup.width;
    let popup_h = state.config.channels.popup.height;
    let always_on_top = state.config.general.always_on_top;
    let window_effect = state.config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    #[cfg(target_os = "macos")]
    let appear_behavior = state
        .config
        .general
        .appear_animation
        .ns_animation_behavior();

    // GUI Helper 模式（Daemon 拉起的弹窗进程）：弹窗是唯一渠道，恒显示窗口；作答经 IPC 回 Daemon。
    let is_helper = popup_ipc.is_some();
    // 方案6 预热弹窗：建窗后隐藏待命、不带请求，由首条 `Show` 领用上屏（延后 show）。
    let warm = popup_ipc.as_ref().map(|i| i.warm).unwrap_or(false);
    // 通道启用判定（仅单进程提问模式使用）。
    let messaging_active = has_active_messaging(&state.config);
    // Helper：恒开弹窗。单进程：弹窗禁用且无可用消息渠道时兜底仍开弹窗，避免进程挂起。
    let show_popup = is_helper || state.config.channels.popup.enabled || !messaging_active;
    // 提问模式下抑制「关窗即退出」：收尾 / 等待 Daemon 收尾时弹窗会先关，需留进程主动退出。
    // 设置模式不抑制，关窗即正常退出。宿主模式恒抑制（窗口全关后是否退出由宿主自身判定）。
    let prevent_autoexit = matches!(view, View::Popup | View::GuiHost);

    crate::perf::mark_env("gui.build_start");
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_drag::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_liquid_glass::init())
        .manage(state)
        .invoke_handler(tauri::generate_handler![
            crate::commands::popup_init,
            crate::commands::enrich_permission_diff,
            crate::commands::perf_mark,
            crate::commands::popup_agent_terminal,
            crate::commands::popup_agent_resolved,
            crate::commands::popup_show_window,
            crate::commands::submit_popup,
            crate::commands::submit_confirm_action,
            crate::commands::confirm_popup_ready,
            crate::commands::cancel_popup,
            crate::commands::open_path,
            crate::commands::preview_attachments,
            crate::commands::close_preview,
            crate::commands::read_image_data_url,
            crate::commands::file_icon_data_url,
            crate::commands::show_attachment_menu,
            crate::commands::get_settings,
            crate::commands::save_settings,
            crate::commands::permission_rules_panel,
            crate::commands::agent_task_workspaces,
            crate::commands::agent_task_workspace_add,
            crate::commands::agent_task_workspace_pick,
            crate::commands::agent_task_workspace_pin,
            crate::commands::agent_task_workspace_hide,
            crate::commands::agent_task_workspace_forget,
            crate::commands::agent_task_readiness,
            crate::commands::agent_task_test_terminal,
            crate::commands::get_prompt,
            crate::commands::collaboration_style_defaults,
            crate::commands::collaboration_style_apply_integrations,
            crate::commands::open_test_popup,
            crate::commands::popup_sound_support,
            crate::commands::play_popup_sound,
            crate::commands::set_theme,
            crate::commands::update_theme,
            crate::commands::open_settings,
            crate::commands::open_agent_console,
            crate::commands::popup_im_tip_visible,
            crate::commands::popup_im_tip_dismiss,
            crate::commands::apply_window_effect,
            crate::commands::start_speech,
            crate::commands::stop_speech,
            crate::commands::flush_speech,
            crate::commands::speech_available,
            crate::commands::cursor_hook_status,
            crate::commands::cursor_hook_install,
            crate::commands::cursor_hook_update,
            crate::commands::cursor_hook_uninstall,
            crate::commands::cursor_hook_reveal,
            crate::commands::claude_hook_status,
            crate::commands::claude_hook_install,
            crate::commands::claude_hook_update,
            crate::commands::claude_hook_uninstall,
            crate::commands::claude_hook_reveal,
            crate::commands::agent_rule_status,
            crate::commands::agent_rule_install,
            crate::commands::agent_rule_update,
            crate::commands::agent_rule_uninstall,
            crate::commands::agent_rule_reveal,
            crate::commands::agent_rule_open,
            crate::commands::agent_mode_status,
            crate::commands::agent_mode_set,
            crate::commands::agent_mode_update,
            crate::commands::agent_mode_update_artifact,
            crate::commands::agent_permission_set,
            crate::commands::agent_stop_set,
            crate::commands::agent_ask_question_set,
            crate::commands::mcp_config_reveal,
            crate::commands::mcp_config_open,
            crate::commands::mcp_command_path,
            crate::commands::agent_hook_reveal,
            crate::commands::agent_hook_open,
            crate::commands::agent_lifecycle_status,
            crate::commands::agent_lifecycle_install,
            crate::commands::agent_lifecycle_uninstall,
            crate::commands::focus_agent_terminal,
            crate::commands::agent_force_idle,
            crate::commands::open_interject,
            crate::commands::interject_init,
            crate::commands::interject_submit,
            crate::commands::interject_cancel,
            crate::commands::interject_clear,
            crate::commands::telegram_test,
            crate::commands::dingtalk_test,
            crate::commands::dingtalk_detect_prepare,
            crate::commands::dingtalk_detect_wait,
            crate::commands::feishu_test,
            crate::commands::feishu_detect_prepare,
            crate::commands::feishu_detect_wait,
            crate::commands::slack_test,
            crate::commands::slack_detect_prepare,
            crate::commands::slack_detect_wait,
            crate::commands::detect_cancel,
            crate::commands::open_history,
            crate::commands::history_init,
            crate::commands::agents_init,
            crate::commands::agents_start_subscription,
            crate::commands::agents_focus,
            crate::commands::focus_request,
            crate::commands::interject_append,
            crate::commands::interject_peek,
            crate::commands::console_transcript,
            crate::commands::console_diff_stat,
            crate::commands::console_diff_file,
            crate::commands::console_stage,
            crate::commands::get_history,
            crate::commands::get_history_projects,
            crate::commands::history_count,
            crate::commands::trim_history,
            crate::commands::delete_history_entries,
            crate::commands::clear_all_history,
            crate::commands::resolve_history_session_titles,
            crate::commands::get_app_version,
            crate::commands::update_check,
            crate::commands::update_get_notes,
            crate::commands::update_get_version_notes,
            crate::commands::update_apply,
            crate::commands::update_dismiss,
            crate::commands::restart_settings,
            crate::commands::popup_update_state,
            crate::commands::channel_health,
            crate::commands::todos_list,
            crate::commands::todos_add,
            crate::commands::todos_update,
            crate::commands::todos_update_attachments,
            crate::commands::todos_attach_pasted_images,
            crate::commands::todo_attachment_thumbnail,
            crate::commands::todos_remove,
            crate::commands::todos_complete,
            crate::commands::todos_clear,
            crate::commands::todos_reorder,
            crate::commands::todos_set_auto,
            crate::commands::todos_set_text,
            crate::commands::todos_history,
            crate::commands::todos_restore,
            crate::commands::todos_history_clear,
            crate::commands::todos_init,
            crate::commands::todos_projects,
            crate::commands::todos_projects_enriched,
            crate::commands::open_todos,
            crate::commands::open_new_task,
            crate::commands::open_fork_task,
            crate::commands::new_task_init,
            crate::commands::new_task_projects,
            crate::commands::new_task_projects_refreshed,
            crate::commands::project_key_of,
            crate::commands::new_task_launch,
            crate::commands::fork_task_init,
            crate::commands::fork_task_launch,
        ])
        .on_window_event(|window, event| {
            match window.label() {
                // 弹窗：关闭即取消 / 记忆尺寸。
                "popup" => match event {
                    WindowEvent::CloseRequested { api, .. } => {
                        use tauri::Emitter;
                        let app = window.app_handle();
                        // 已在收尾（提交/取消触发的 w.close()）→ 放行关闭。
                        let finishing = app
                            .try_state::<GuiBridge>()
                            .map(|b| b.is_done())
                            .unwrap_or(false)
                            || app
                                .try_state::<Arc<Coordinator>>()
                                .map(|c| c.is_finalizing())
                                .unwrap_or(false);
                        if !finishing {
                            // 原生关闭按钮：与 ⌘W 一致——阻止本次关闭，交前端决定（有输入则二次确认）。
                            api.prevent_close();
                            let _ = app.emit("popup-close-requested", ());
                        }
                    }
                    WindowEvent::Resized(_) => persist_popup_size(window),
                    WindowEvent::Focused(true) => {
                        if let Some(bridge) = window.app_handle().try_state::<GuiBridge>() {
                            bridge.send_popup_focused();
                        }
                    }
                    WindowEvent::Destroyed => {
                        if let Some(bridge) = window.app_handle().try_state::<GuiBridge>() {
                            bridge.send_popup_dismissed();
                        }
                    }
                    _ => {}
                },
                // 设置窗口关闭时清掉 Liquid Glass 注册表条目：插件按 label 缓存玻璃视图，
                // 若不清理，下次同 label 重开会走 update 分支去操作已销毁的旧视图，导致背景透明无玻璃。
                #[cfg(target_os = "macos")]
                l if gui_host::is_hosted_label(l) => {
                    if matches!(event, WindowEvent::CloseRequested { .. }) {
                        clear_window_glass(window);
                    }
                }
                _ => {}
            }
            if matches!(event, WindowEvent::Destroyed) && gui_host::is_hosted_label(window.label())
            {
                // 插话窗口销毁 → 关闭其 composer 连接（daemon 视为「composer 关闭」，放行等待 hook）。
                // 兜底路径：正常取消/提交已由命令关闭，这里覆盖直接关窗/进程内异常。
                if window.label().starts_with("interject-") {
                    crate::client::composer::close_by_label(window.label());
                }
                // 宿主模式：托管窗口销毁后重算窗口计数（驱动 daemon 续命与宿主退出判定）。
                let app = window.app_handle();
                if app.try_state::<gui_host::HostState>().is_some() {
                    gui_host::recount_windows(app);
                }
            }
        })
        .on_menu_event(|app, event| {
            // 托盘菜单事件仅在宿主进程内有 HostState；其余进程无托盘、忽略。
            if app.try_state::<gui_host::HostState>().is_some() {
                gui_host::on_menu_event(app, event.id().as_ref());
            }
        })
        .setup(move |app| {
            // 方案6：预热弹窗待命期不该入坞——尽早设 accessory（在设 Dock 图标 / 建窗前），避免常驻 Dock 图标。
            // 领用上屏时 `finalize_popup_show` 再切回 Regular，使弹窗像冷路径一样入坞。
            #[cfg(target_os = "macos")]
            if warm {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            // 裸二进制运行时 Dock 不会用 bundle 图标；运行时显式覆盖（仅影响本进程）。
            #[cfg(target_os = "macos")]
            crate::macos_dock_icon::set_dock_icon();
            match view {
                View::Popup => {
                    // Dock 跳动 + 角标提问数（冷路径有请求；预热路径延后到 `popup_show_window` 领用时）。
                    #[cfg(target_os = "macos")]
                    if !warm && !is_helper {
                        let count = app
                            .state::<AppState>()
                            .interaction
                            .ask()
                            .map(|request| request.questions.len())
                            .unwrap_or(1);
                        crate::macos_dock_icon::announce_questions(count);
                    }

                    if show_popup || warm {
                        let mut url = String::from("index.html?view=popup");
                        append_window_effect_query(&mut url, effective_window_effect);
                        let builder =
                            WebviewWindowBuilder::new(app, "popup", WebviewUrl::App(url.into()))
                                .title(i18n::tr(lang, "title.popup"))
                                .inner_size(popup_w, popup_h)
                                .min_inner_size(420.0, 480.0)
                                .center()
                                // 先隐藏构建，设好原生出现动画后再显示，触发 macOS 窗口出现动画。
                                .visible(false)
                                .focused(false)
                                .always_on_top(always_on_top)
                                // 方案6：禁用 WebView 后台节流，使隐藏/被遮挡时 rAF/定时器照常回调。预热窗长期隐藏；
                                // 且「内容绘制完成才 show()」依赖双 rAF，默认 Suspend 会暂停回调 → 永不上屏。
                                .background_throttling(
                                    tauri::utils::config::BackgroundThrottlingPolicy::Disabled,
                                )
                                .theme(theme);
                        let win =
                            apply_surface(builder, window_bg, effective_window_effect).build()?;
                        #[cfg(target_os = "macos")]
                        set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
                        // Todos may be added from the separate manager window, CLI, MCP, or IM
                        // while this question is open. Keep the popup's project list live.
                        watch_todos_file(win.clone());
                        // 预热路径：窗口保持隐藏待命，待 `Show` 领用、前端绘制完成后由 `popup_show_window` 上屏。
                        if !warm && !is_helper {
                            // macOS：隐藏构建后先设原生出现动画（样式由设置决定），再 show()。
                            #[cfg(target_os = "macos")]
                            if let Ok(ns) = win.ns_window() {
                                crate::macos_window_anim::set_appear_animation(ns, appear_behavior);
                            }
                            let _ = win.show();
                            crate::perf::mark_env("gui.win_show");
                            // Play the configured popup sound after the window becomes visible.
                            crate::sound::play(&app.state::<AppState>().config.general.popup_sound);
                        }
                    }

                    match popup_ipc {
                        // —— GUI Helper 模式：作答经 IPC 回 Daemon，无本地协调器 / 消息渠道 ——
                        Some(ipc) => {
                            let PopupIpc {
                                gui_tx,
                                request_id,
                                reader,
                                warm: _,
                            } = ipc;
                            app.manage(GuiBridge {
                                tx: gui_tx,
                                request_id: std::sync::Mutex::new(request_id),
                                done: AtomicBool::new(false),
                                ready_sent: AtomicBool::new(false),
                                presented: AtomicBool::new(false),
                                app: app.handle().clone(),
                            });
                            // 方案6 预热：manage 领用槽（None=待命）；首条 `Show` 经 reader 循环填入并唤醒前端。
                            if warm {
                                app.manage(WarmPopup {
                                    show: std::sync::Mutex::new(None),
                                    finalized: AtomicBool::new(false),
                                });
                            }
                            // 读 Daemon → GUI 的消息：被抢答 cancel / 连接断开 → 退出本进程。
                            let app_handle = app.handle().clone();
                            tauri::async_runtime::spawn(async move {
                                let mut reader = reader;
                                loop {
                                    match crate::ipc::read_msg::<_, crate::ipc::ServerMsg>(
                                        &mut reader,
                                    )
                                    .await
                                    {
                                        // 方案6 预热领用：首条 `Show` 把请求注入已挂载的待命弹窗。
                                        // 回填 GuiBridge.request_id + 存入领用槽，再 emit 唤醒前端拉取渲染
                                        //（前端 pull `popup_init` 取已领用请求 → 绘制 → 调 `popup_show_window` 上屏）。
                                        Ok(Some(crate::ipc::ServerMsg::Show(show))) => {
                                            use tauri::{Emitter, Manager};
                                            // 方案6 埋点：热 helper 无 perf env，领用时由 Show 注入 perf 上下文，
                                            // 使其 fe.painted/gui.win_show 与 CLI 同 perf_id 关联。
                                            crate::perf::set_runtime(
                                                &show.perf_id,
                                                show.perf_autodismiss,
                                            );
                                            crate::perf::mark_env("gui.show_recv");
                                            if let Some(bridge) =
                                                app_handle.try_state::<GuiBridge>()
                                            {
                                                bridge.set_request_id(show.request_id.clone());
                                            }
                                            if let Some(warm_state) =
                                                app_handle.try_state::<WarmPopup>()
                                            {
                                                *warm_state.show.lock().unwrap() = Some(show);
                                            }
                                            let _ = app_handle.emit("popup-show", ());
                                        }
                                        Ok(Some(crate::ipc::ServerMsg::PresentPopup {
                                            request_id,
                                            presentation,
                                        })) => {
                                            let matches = app_handle
                                                .try_state::<GuiBridge>()
                                                .map(|bridge| bridge.request_id() == request_id)
                                                .unwrap_or(false);
                                            if !matches {
                                                continue;
                                            }
                                            let app2 = app_handle.clone();
                                            let _ = app_handle.run_on_main_thread(move || {
                                                finalize_popup_show(&app2, presentation);
                                            });
                                        }
                                        Ok(Some(crate::ipc::ServerMsg::Cancel { .. })) => {
                                            let app2 = app_handle.clone();
                                            let _ = app_handle.run_on_main_thread(move || {
                                                if let Some(bridge) = app2.try_state::<GuiBridge>()
                                                {
                                                    bridge.dismiss_from_daemon();
                                                }
                                            });
                                        }
                                        // 配置实时变更（A12）：转发给前端实时切主题/语言。
                                        // 复用既有 "settings-updated" 事件（前端已监听 general 配置）。
                                        Ok(Some(crate::ipc::ServerMsg::ConfigChanged {
                                            general,
                                        })) => {
                                            use tauri::Emitter;
                                            // 先同步原生窗口外观：玻璃/毛玻璃材质随 NSAppearance 切换，
                                            // 仅靠前端 CSS 会出现「网页变浅、窗体仍深」（见 A12 实测）。
                                            if let Some(theme) =
                                                general.get("theme").and_then(|t| t.as_str())
                                            {
                                                crate::commands::apply_theme_to_windows(
                                                    &app_handle,
                                                    theme,
                                                );
                                            }
                                            // Hot-sync the requested material to the in-flight helper.
                                            //（热待命进程不在 broadcast 列表，靠 finalize 领用时兜底）。
                                            // apply_window_effect_to_all 内部 hop 主线程（本 reader 在 tokio worker）。
                                            if let Some(effect) = general
                                                .get("windowEffect")
                                                .and_then(|v| v.as_str())
                                                .and_then(parse_window_effect)
                                            {
                                                apply_window_effect_to_all(&app_handle, effect);
                                            }
                                            let _ = app_handle.emit("settings-updated", general);
                                        }
                                        // 版本自更新态（D→GUI）：缓存进程内 + emit 给弹窗前端
                                        // （弹窗挂载先 pull `popup_update_state` 取初值，再靠此事件实时更新）。
                                        Ok(Some(crate::ipc::ServerMsg::UpdateState {
                                            available,
                                            latest_version,
                                            pending,
                                        })) => {
                                            use tauri::Emitter;
                                            let payload = crate::commands::PushedUpdateState {
                                                available,
                                                latest_version,
                                                pending,
                                            };
                                            crate::commands::set_pushed_update(payload.clone());
                                            let _ = app_handle.emit("update-state", payload);
                                        }
                                        // 调用方 agent 异步解析结果（D→GUI，方案5/b）：缓存进程内 + emit
                                        // 给弹窗前端（弹窗挂载先 pull `popup_agent_resolved` 取初值，再靠
                                        // 此事件实时升级 badge / 「聚焦终端」）。
                                        Ok(Some(crate::ipc::ServerMsg::AgentResolved {
                                            kind,
                                            pid,
                                        })) => {
                                            use tauri::Emitter;
                                            let payload =
                                                crate::commands::PushedAgent { kind, pid };
                                            crate::commands::set_pushed_agent(payload.clone());
                                            let _ = app_handle.emit("agent-resolved", payload);
                                        }
                                        // 托盘「待答」子菜单点击：聚焦本弹窗并通知前端闪烁边框。
                                        Ok(Some(crate::ipc::ServerMsg::FocusPopup { .. })) => {
                                            use tauri::Emitter;
                                            let app2 = app_handle.clone();
                                            let _ = app_handle.run_on_main_thread(move || {
                                                if let Some(win) = app2.get_webview_window("popup")
                                                {
                                                    let _ = win.set_focus();
                                                }
                                                let _ = app2.emit("popup-flash", ());
                                            });
                                        }
                                        Ok(Some(_)) => {}
                                        Ok(None) | Err(_) => {
                                            app_handle.exit(0);
                                            break;
                                        }
                                    }
                                }
                            });
                        }
                        // —— 单进程模式（非 unix 回退）：协调器 + 弹窗 Channel + 并行消息渠道 ——
                        None => {
                            let request = app.state::<AppState>().ask_request().clone();
                            let project = app.state::<AppState>().project.clone();
                            let source = app.state::<AppState>().source.clone();
                            let agent_kind = app.state::<AppState>().agent_kind.clone();
                            let origin = crate::channels::ConversationOrigin::new(
                                &source,
                                agent_kind.as_deref(),
                                &project,
                            );
                            let coordinator = Coordinator::new(
                                app.handle().clone(),
                                request.clone(),
                                project,
                                source,
                                agent_kind,
                            );
                            if show_popup {
                                coordinator
                                    .register(Arc::new(PopupChannel::new(app.handle().clone())));
                            }
                            let config = app.state::<AppState>().config.clone();
                            for ch in active_messaging_channels(&config) {
                                coordinator.register(ch.clone());
                                ch.start(&request, &origin, coordinator.clone());
                            }
                            app.manage(coordinator);
                        }
                    }
                }
                View::Settings => {
                    // Window build only needs general (theme); get_settings() reads secrets later.
                    let config = AppConfig::load_without_secrets();
                    // 独立 --settings 进程内无弹窗 → 不置顶（popup_pin 恒 false）。
                    create_settings_window(app, &config, popup_pin(app, &config), None)?;
                }
                View::History { all } => {
                    // History window only needs general (theme); skip keychain.
                    let config = AppConfig::load_without_secrets();
                    // 进程内默认项目（AppState.project = CLI 探测的当前项目）→ 传 None 沿用。
                    create_history_window(app, &config, all, None, None, popup_pin(app, &config))?;
                }
                View::Todos => {
                    let config = AppConfig::load_without_secrets();
                    // 预选项目在 AppState.project（CLI 探测的 cwd git 根）。
                    let project = app.state::<AppState>().project.clone();
                    let preselect = (!project.is_empty()).then_some(project.as_str());
                    create_todos_window(app, &config, preselect, popup_pin(app, &config))?;
                }
                View::GuiHost => {
                    let config = AppConfig::load_without_secrets();
                    gui_host::setup(app, &config)?;
                }
                View::Agents => {
                    let config = AppConfig::load_without_secrets();
                    create_agents_window(app, &config, None, false)?;
                    // 订阅不在此处启动：daemon 一连上就推一帧立即快照，若现在就连，emit 会早于
                    // 前端注册 `agents-updated` 监听（Tauri 事件不缓存）而丢首帧，窗口空等到下一次
                    // 周期推送（15s 内随机）。改由前端挂载、监听就绪后经 `agents_start_subscription`
                    // 命令触发，保证首帧必被收到。
                }
            }
            Ok(())
        })
        .build(tauri::generate_context!())?;
    crate::perf::mark_env("gui.build_done");

    // 构建成功后、进入事件循环前静默系统噪音日志（如 macOS 的 TSM CapsLock 日志）。
    stderr_redirect::silence();
    app.run(move |app_handle, event| {
        // 宿主模式：托管窗口全关也不退出（是否退出由宿主自身 evaluate_exit 经 app.exit() 决定）。
        // 故拦下一切「关窗触发」的退出（code=None）；宿主主动退出走 app.exit(code) → code=Some 放行。
        if app_handle.try_state::<gui_host::HostState>().is_some() {
            if let RunEvent::ExitRequested { code, api, .. } = &event {
                if code.is_none() {
                    api.prevent_exit();
                }
            }
            return;
        }
        // 提问模式：拦下关窗触发的退出（code=None），由协调器 / GUI Helper 逻辑决定真正退出时机。
        // 设置模式不拦，关窗即正常退出。
        if prevent_autoexit {
            if let RunEvent::ExitRequested { code, api, .. } = &event {
                if code.is_none() {
                    if let Some(bridge) = app_handle.try_state::<GuiBridge>() {
                        // GUI Helper：关窗 / Cmd+Q → 通知 Daemon 取消，等其收尾关闭连接后由
                        // reader 驱动 `app.exit(0)`（或安全网超时），确保取消已送达 Daemon。
                        api.prevent_exit();
                        bridge.send_cancel();
                    } else {
                        // 单进程：仅在收尾阶段拦下，放行协调器 `app.exit(code)` 先输出结果。
                        let finalizing = app_handle
                            .try_state::<Arc<Coordinator>>()
                            .map(|c| c.is_finalizing())
                            .unwrap_or(false);
                        if finalizing {
                            api.prevent_exit();
                        }
                    }
                }
            }
        }
    });
    std::process::exit(0);
}

/// 渲染结果：把一个终态 `ChannelResult` 转成「给 stdout 的文本 / 给 stderr 的错误 + 退出码」。
///
/// 纯函数（除图片落盘的 IO 外），不打印、不退出，便于 Daemon 复用后经 IPC 回传 CLI。
/// 单进程路径由 `emit_result` 包一层做实际打印 / 退出。
#[derive(Debug, Clone)]
pub struct RenderOutcome {
    /// 给 CLI stdout 的结果区块文本（不含尾换行；打印方负责换行）。
    pub stdout: String,
    /// 给 CLI stderr 的错误文本（仅错误路径有值；含 `Error:` 前缀）。
    pub stderr: Option<String>,
    /// 退出码：0（发送/取消正常）/ 1（落盘等错误）。
    pub exit_code: i32,
}

/// 渲染终态结果（图片落盘到 `temp/askhuman/<request_id>/`）。文案按传入 `lang` 本地化。
///
/// 第二个返回值为**各题已落盘图片路径**（取消路径为空），供回复历史按路径记录复用；调用方
/// 通常只用第一个 `RenderOutcome`。
pub(crate) fn render_result(
    request: &AskRequest,
    result: &ChannelResult,
    lang: Lang,
) -> (RenderOutcome, Vec<Vec<String>>) {
    use crate::models::OutputFormat;
    // whats-next（spec todo-whats-next D3）：stdout 为一段纯文本（任务内容 / 固定结束句），
    // 取消沿用 `[status]`；附件仍落盘并以 `[files]` 附于文本后。
    if request.whats_next {
        return render_whats_next(request, result, lang);
    }
    let json = request.output_format == OutputFormat::Json;
    match result.action {
        ChannelAction::Cancel => (
            RenderOutcome {
                stdout: if json {
                    output::render_json(request, result, &[], lang)
                } else {
                    output::cancel_output(lang)
                },
                stderr: None,
                exit_code: 0,
            },
            Vec::new(),
        ),
        ChannelAction::Send => {
            // 逐题落盘图片（按题分子目录避免文件名冲突），再聚合输出。
            let mut image_paths_per_q: Vec<Vec<String>> = Vec::with_capacity(result.answers.len());
            for (i, answer) in result.answers.iter().enumerate() {
                match image_writer::save(&answer.images, &request.id, i, lang) {
                    Ok(paths) => image_paths_per_q.push(paths),
                    Err(e) => {
                        return (
                            RenderOutcome {
                                stdout: String::new(),
                                stderr: Some(format!("{}{}", i18n::err_prefix(lang), e)),
                                exit_code: 1,
                            },
                            Vec::new(),
                        );
                    }
                }
            }

            let stdout = if json {
                output::render_json(request, result, &image_paths_per_q, lang)
            } else {
                let rendered: Vec<output::RenderedAnswer> = result
                    .answers
                    .iter()
                    .enumerate()
                    .map(|(i, answer)| output::RenderedAnswer {
                        selected_options: &answer.selected_options,
                        user_input: answer.user_input.as_deref(),
                        image_paths: &image_paths_per_q[i],
                        file_paths: &answer.files,
                    })
                    .collect();
                output::aggregate_output(lang, &rendered)
            };

            (
                RenderOutcome {
                    stdout,
                    stderr: None,
                    exit_code: 0,
                },
                image_paths_per_q,
            )
        }
    }
}

/// whats-next 结果渲染（spec todo-whats-next D3）：提交映射（`output::whats_next_reply`）→
/// 一段纯文本；回答附带的图片照常落盘，与透传文件一起按 `[files]` 附于文本后。
fn render_whats_next(
    request: &AskRequest,
    result: &ChannelResult,
    lang: Lang,
) -> (RenderOutcome, Vec<Vec<String>>) {
    let reply = output::whats_next_reply(request, result);
    // 落盘图片（仅 Send 路径有回答；取消路径 answers 为空，循环自然跳过）。
    let mut image_paths_per_q: Vec<Vec<String>> = Vec::with_capacity(result.answers.len());
    for (i, answer) in result.answers.iter().enumerate() {
        match image_writer::save(&answer.images, &request.id, i, lang) {
            Ok(paths) => image_paths_per_q.push(paths),
            Err(e) => {
                return (
                    RenderOutcome {
                        stdout: String::new(),
                        stderr: Some(format!("{}{}", i18n::err_prefix(lang), e)),
                        exit_code: 1,
                    },
                    Vec::new(),
                );
            }
        }
    }
    let files: Vec<String> = image_paths_per_q
        .iter()
        .flatten()
        .cloned()
        .chain(result.answers.iter().flat_map(|a| a.files.iter().cloned()))
        .collect();
    (
        RenderOutcome {
            stdout: output::whats_next_output(&reply, &files, lang),
            stderr: None,
            exit_code: 0,
        },
        image_paths_per_q,
    )
}

/// 把结果输出到 stdout（或 stderr），返回退出码。（保留供复用；当前协调器内联渲染。）
pub(crate) fn emit_result(request: &AskRequest, result: &ChannelResult) -> i32 {
    let (outcome, _) = render_result(request, result, Lang::current());
    if let Some(err) = &outcome.stderr {
        stderr_redirect::eprintln_real(err);
    } else {
        println!("{}", outcome.stdout);
    }
    outcome.exit_code
}

/// 解析“实际”主题：system 时探测系统深/浅色。
fn resolved_theme(config: &AppConfig) -> tauri::Theme {
    match config.general.theme {
        ThemeMode::Light => tauri::Theme::Light,
        ThemeMode::Dark => tauri::Theme::Dark,
        ThemeMode::System => match dark_light::detect() {
            Ok(dark_light::Mode::Dark) => tauri::Theme::Dark,
            _ => tauri::Theme::Light,
        },
    }
}

/// Resolve the persisted preference into a material supported by the current macOS runtime.
fn resolve_window_effect(requested: WindowEffect, glass_supported: bool) -> WindowEffect {
    match requested {
        WindowEffect::Glass if !glass_supported => WindowEffect::Blur,
        other => other,
    }
}

#[cfg(target_os = "macos")]
fn glass_supported() -> bool {
    objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_some()
}

fn effective_window_effect(requested: WindowEffect) -> WindowEffect {
    #[cfg(target_os = "macos")]
    {
        resolve_window_effect(requested, glass_supported())
    }
    #[cfg(not(target_os = "macos"))]
    {
        requested
    }
}

/// Add the effective material to an internal window URL so first-frame CSS is correct.
fn append_window_effect_query(url: &mut String, effect: WindowEffect) {
    #[cfg(target_os = "macos")]
    {
        url.push(if url.contains('?') { '&' } else { '?' });
        url.push_str("effect=");
        url.push_str(effect.as_str());
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (url, effect);
}

/// Platform-specific initial surface:
/// - macOS Blur gets Tauri's native `UnderWindowBackground` effect at build time;
/// - macOS Glass stays transparent until the plugin attaches `NSGlassEffectView` after build;
/// - macOS Solid starts with the current theme color and no Visual Effects view;
/// - other platforms keep their existing opaque background.
fn apply_surface<'a, R, M>(
    builder: WebviewWindowBuilder<'a, R, M>,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] window_bg: tauri::window::Color,
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))] effect: WindowEffect,
) -> WebviewWindowBuilder<'a, R, M>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    #[cfg(target_os = "macos")]
    {
        let builder = builder
            .transparent(true)
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
        match effect {
            WindowEffect::Blur => builder.effects(
                EffectsBuilder::new()
                    .effect(Effect::UnderWindowBackground)
                    .state(EffectState::FollowsWindowActiveState)
                    .build(),
            ),
            WindowEffect::Glass => builder,
            WindowEffect::Solid => builder.background_color(window_bg),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        builder.background_color(window_bg)
    }
}

#[cfg(target_os = "macos")]
fn apply_liquid_glass<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> Result<(), String> {
    use tauri_plugin_liquid_glass::{LiquidGlassConfig, LiquidGlassExt};
    window
        .liquid_glass()
        .set_effect(window, LiquidGlassConfig::default())
        .map_err(|error| error.to_string())
}

/// 窗口关闭前移除 Liquid Glass 背景：同时把插件按 label 缓存的注册表条目清掉，
/// 以便同 label 窗口下次重建时能重新走「create」分支挂上玻璃。须在视图仍存活时调用。
#[cfg(target_os = "macos")]
fn clear_window_glass(window: &tauri::Window) {
    use tauri_plugin_liquid_glass::{LiquidGlassConfig, LiquidGlassExt};
    if let Some(w) = window.app_handle().get_webview_window(window.label()) {
        if let Err(error) = w.liquid_glass().set_effect(
            &w,
            LiquidGlassConfig {
                enabled: false,
                ..Default::default()
            },
        ) {
            stderr_redirect::eprintln_real(&format!(
                "window material cleanup failed: window={} error={error}",
                window.label()
            ));
        }
    }
}

#[cfg(target_os = "macos")]
fn disable_plugin_effect<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
) -> Result<(), String> {
    use tauri_plugin_liquid_glass::{LiquidGlassConfig, LiquidGlassExt};
    window
        .liquid_glass()
        .set_effect(
            window,
            LiquidGlassConfig {
                enabled: false,
                ..Default::default()
            },
        )
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn remove_native_blur_views<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    if let Ok(ns) = window.ns_window() {
        crate::macos_window_anim::remove_vibrancy_views(ns);
    }
}

#[cfg(target_os = "macos")]
fn remove_all_native_effect_views<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) {
    if let Ok(ns) = window.ns_window() {
        crate::macos_window_anim::remove_window_effect_views(ns);
    }
}

#[cfg(target_os = "macos")]
fn set_native_opaque<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>, opaque: bool) {
    if let Ok(ns) = window.ns_window() {
        crate::macos_window_anim::set_window_opaque(ns, opaque);
    }
}

#[cfg(target_os = "macos")]
fn apply_native_glass<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> Result<(), String> {
    set_native_opaque(window, false);
    window
        .set_background_color(Some(tauri::window::Color(0, 0, 0, 0)))
        .map_err(|error| error.to_string())?;
    remove_native_blur_views(window);
    apply_liquid_glass(window)
}

#[cfg(target_os = "macos")]
fn apply_native_blur<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>) -> Result<(), String> {
    disable_plugin_effect(window)?;
    set_native_opaque(window, false);
    window
        .set_background_color(Some(tauri::window::Color(0, 0, 0, 0)))
        .map_err(|error| error.to_string())?;
    remove_native_blur_views(window);
    window
        .set_effects(
            EffectsBuilder::new()
                .effect(Effect::UnderWindowBackground)
                .state(EffectState::FollowsWindowActiveState)
                .build(),
        )
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "macos")]
fn apply_solid<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    window_bg: tauri::window::Color,
) -> Result<(), String> {
    let cleanup_result = disable_plugin_effect(window);
    remove_all_native_effect_views(window);
    let background_result = window
        .set_background_color(Some(window_bg))
        .map_err(|error| error.to_string());
    set_native_opaque(window, true);
    cleanup_result.and(background_result)
}

#[cfg(target_os = "macos")]
fn log_material_error<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    requested: WindowEffect,
    effective: WindowEffect,
    stage: &str,
    error: &str,
) {
    stderr_redirect::eprintln_real(&format!(
        "window material failed: window={} requested={} effective={} stage={} error={}",
        window.label(),
        requested.as_str(),
        effective.as_str(),
        stage,
        error
    ));
}

#[cfg(target_os = "macos")]
fn emit_window_effect<R: tauri::Runtime>(window: &tauri::WebviewWindow<R>, effect: WindowEffect) {
    use tauri::Emitter;
    if let Err(error) = window.emit("window-effect-changed", effect.as_str()) {
        stderr_redirect::eprintln_real(&format!(
            "window material event failed: window={} effect={} error={error}",
            window.label(),
            effect.as_str()
        ));
    }
}

#[cfg(target_os = "macos")]
fn set_runtime_window_effect_with_bg<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    requested: WindowEffect,
    window_bg: tauri::window::Color,
) -> WindowEffect {
    let effective = effective_window_effect(requested);
    let actual = match effective {
        WindowEffect::Glass => match apply_native_glass(window) {
            Ok(()) => WindowEffect::Glass,
            Err(error) => {
                log_material_error(window, requested, effective, "glass", &error);
                match apply_native_blur(window) {
                    Ok(()) => WindowEffect::Blur,
                    Err(error) => {
                        log_material_error(window, requested, effective, "blur-fallback", &error);
                        if let Err(error) = apply_solid(window, window_bg) {
                            log_material_error(
                                window,
                                requested,
                                effective,
                                "solid-fallback",
                                &error,
                            );
                        }
                        WindowEffect::Solid
                    }
                }
            }
        },
        WindowEffect::Blur => match apply_native_blur(window) {
            Ok(()) => WindowEffect::Blur,
            Err(error) => {
                log_material_error(window, requested, effective, "blur", &error);
                if let Err(error) = apply_solid(window, window_bg) {
                    log_material_error(window, requested, effective, "solid-fallback", &error);
                }
                WindowEffect::Solid
            }
        },
        WindowEffect::Solid => {
            if let Err(error) = apply_solid(window, window_bg) {
                log_material_error(window, requested, effective, "solid", &error);
            }
            WindowEffect::Solid
        }
    };
    emit_window_effect(window, actual);
    actual
}

#[cfg(target_os = "macos")]
pub(crate) fn set_runtime_window_effect<R: tauri::Runtime>(
    window: &tauri::WebviewWindow<R>,
    requested: WindowEffect,
) {
    let config = AppConfig::load_without_secrets();
    let window_bg = background_for(resolved_theme(&config));
    set_runtime_window_effect_with_bg(window, requested, window_bg);
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_runtime_window_effect<R: tauri::Runtime>(
    _window: &tauri::WebviewWindow<R>,
    _requested: WindowEffect,
) {
}

/// 对本进程内**全部** WebView 窗口套用窗口背景效果（设置页即时切换 + ConfigChanged 热同步）。
///
/// **必须 hop 到主线程**：AppKit 的 `removeFromSuperview` / `NSVisualEffectView` /
/// Liquid Glass 视图层级操作在非主线程会触发 AutoLayout 断言并 abort（实测 blur→glass 崩在
/// tokio-rt-worker）。调用方可在任意线程；本函数只把闭包投递到主 runloop，不阻塞等待。
pub(crate) fn apply_window_effect_to_all<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    effect: WindowEffect,
) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        use tauri::Manager;
        for (_label, w) in app2.webview_windows() {
            set_runtime_window_effect(&w, effect);
        }
    });
}

/// Refresh the native safety background after a theme change while Solid is active.
#[cfg(target_os = "macos")]
pub(crate) fn refresh_solid_window_backgrounds<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    window_bg: tauri::window::Color,
) {
    let app2 = app.clone();
    let _ = app.run_on_main_thread(move || {
        use tauri::Manager;
        for (_label, window) in app2.webview_windows() {
            if let Err(error) = window.set_background_color(Some(window_bg)) {
                stderr_redirect::eprintln_real(&format!(
                    "solid window background refresh failed: window={} error={error}",
                    window.label()
                ));
            }
            set_native_opaque(&window, true);
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn refresh_solid_window_backgrounds<R: tauri::Runtime>(
    _app: &tauri::AppHandle<R>,
    _window_bg: tauri::window::Color,
) {
}

/// Parse the persisted `windowEffect` value from a general-config broadcast.
pub(crate) fn parse_window_effect(s: &str) -> Option<WindowEffect> {
    match s {
        "glass" => Some(WindowEffect::Glass),
        "blur" => Some(WindowEffect::Blur),
        "solid" => Some(WindowEffect::Solid),
        _ => None,
    }
}

#[cfg(test)]
mod window_effect_tests {
    use super::*;

    #[test]
    fn resolves_requested_material_against_glass_capability() {
        assert_eq!(
            resolve_window_effect(WindowEffect::Glass, true),
            WindowEffect::Glass
        );
        assert_eq!(
            resolve_window_effect(WindowEffect::Glass, false),
            WindowEffect::Blur
        );
        for supported in [false, true] {
            assert_eq!(
                resolve_window_effect(WindowEffect::Blur, supported),
                WindowEffect::Blur
            );
            assert_eq!(
                resolve_window_effect(WindowEffect::Solid, supported),
                WindowEffect::Solid
            );
        }
    }

    #[test]
    fn parses_all_persisted_material_values() {
        assert_eq!(parse_window_effect("glass"), Some(WindowEffect::Glass));
        assert_eq!(parse_window_effect("blur"), Some(WindowEffect::Blur));
        assert_eq!(parse_window_effect("solid"), Some(WindowEffect::Solid));
        assert_eq!(parse_window_effect("unknown"), None);
    }
}

/// 「辅助窗口是否应浮于置顶弹窗之上」的进程内判定：当前进程内存在 popup 窗口且弹窗置顶。
/// 仅适用于弹窗助手进程（弹窗与辅助窗口同进程）；统一 GUI 宿主里弹窗在另一进程，需另行判定。
pub(crate) fn popup_pin<R, M>(manager: &M, config: &AppConfig) -> bool
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    manager.get_webview_window("popup").is_some() && config.general.always_on_top
}

/// 创建（或聚焦已存在的）设置窗口。供 `--settings` 启动与弹窗导航栏共用。
///
/// `pin_above_popup`：是否让窗口与置顶弹窗同级，确保新建获焦后浮于弹窗之上。由调用方判定——
/// 弹窗进程内建窗时为「本进程有 popup 且弹窗置顶」（见 [`popup_pin`]）；统一 GUI 宿主里
/// 弹窗在**另一进程**，宿主据 daemon 在途请求数 + 置顶配置自行判定（见 `app::gui_host`）。
pub(crate) fn create_settings_window<R, M>(
    manager: &M,
    config: &AppConfig,
    pin_above_popup: bool,
    initial_tab: Option<&str>,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    if let Some(w) = manager.get_webview_window("settings") {
        let _ = w.set_focus();
        // 已开窗：经事件让前端切到目标 tab（前端 mount 时已注册监听）。
        if let Some(tab) = initial_tab {
            use tauri::Emitter;
            let _ = w.emit("settings-goto-tab", tab.to_string());
        }
        return Ok(());
    }
    let theme = window_theme(config);
    let lang = Lang::resolve(&config.general.language);
    let window_bg = background_for(resolved_theme(config));
    // 新开窗：目标 tab 进初始 URL（无监听时序问题）。tab 值可带 `#elementId` 锚点后缀
    // （spec gui-agent-task-launch G5），必须转义避免 `#` 被当作 URL fragment 吞掉后续参数。
    let mut url = match initial_tab {
        Some(tab) => format!("index.html?view=settings&tab={}", urlencode(tab)),
        None => "index.html?view=settings".to_string(),
    };
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    append_window_effect_query(&mut url, effective_window_effect);
    let builder = WebviewWindowBuilder::new(manager, "settings", WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.settings"))
        .inner_size(560.0, 640.0)
        // 最小宽度：保证标题栏内居中的 tab 不会与左上角红绿灯重叠。
        .min_inner_size(480.0, 520.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let win = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
    Ok(())
}

/// 创建（或聚焦已存在的）独立历史窗口。供 `--history` 启动与弹窗导航栏共用。
/// `all` 为 true 时窗口默认展示全部项目（经 URL 参数传递）。
/// `project_override` 为 Some 时（宿主路由场景）携带调用方项目 key，让窗口默认过滤到该项目，
/// 而非宿主进程自身的 `AppState.project`（宿主 cwd 通常无意义）；None 则沿用进程内默认。
/// `pin_above_popup`：是否浮于置顶弹窗之上（语义同 [`create_settings_window`]，由调用方判定）。
pub(crate) fn create_history_window<R, M>(
    manager: &M,
    config: &AppConfig,
    all: bool,
    project_override: Option<&str>,
    history_target: Option<&crate::gui_host::HistoryOpenTarget>,
    pin_above_popup: bool,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    if let Some(w) = manager.get_webview_window("history") {
        use tauri::Emitter;
        let _ = w.emit(
            "history-open-target",
            crate::gui_host::HistoryOpenRequest {
                all,
                project: project_override.map(str::to_string),
                target: history_target.cloned(),
            },
        );
        let _ = w.set_focus();
        return Ok(());
    }
    let theme = window_theme(config);
    let lang = Lang::resolve(&config.general.language);
    let window_bg = background_for(resolved_theme(config));
    // 基础 URL；`all` 与 `project` 经 query 传给前端（前端 onMounted 据此设默认过滤）。
    let mut url = String::from("index.html?view=history");
    if all {
        url.push_str("&all=1");
    }
    if let Some(key) = project_override {
        // 携带项目 key + 预算好的展示名（避免前端再算 basename）；空串=未知项目（仍带参数以区分「未传」）。
        url.push_str("&project=");
        url.push_str(&urlencode(key));
        url.push_str("&projectName=");
        url.push_str(&urlencode(&crate::project::display_name(key)));
    }
    if let Some(target) = history_target {
        if let Ok(json) = serde_json::to_string(target) {
            url.push_str("&historyTarget=");
            url.push_str(&urlencode(&json));
        }
    }
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    append_window_effect_query(&mut url, effective_window_effect);
    let builder = WebviewWindowBuilder::new(manager, "history", WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.history"))
        .inner_size(820.0, 600.0)
        .min_inner_size(600.0, 440.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let win = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
    // 监听 history.jsonl 变更 → 通知历史窗口实时重载（写入方在别的进程，靠文件监听跨进程感知）。
    watch_history_file(win);
    Ok(())
}

/// 监听历史文件变更并向历史窗口发 `history-updated`（前端据此重载，保留当前选中条目）。
/// 写临时文件 + rename 会换 inode，故监听**配置目录**再按文件名过滤最稳（与 config_watch 同思路）。
fn watch_history_file<R: tauri::Runtime>(window: tauri::WebviewWindow<R>) {
    use tauri::Emitter;
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::Duration;
        let dir = crate::paths::config_dir();
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = channel::<()>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let hit = ev
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n == "history.jsonl").unwrap_or(false));
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
        // 去抖：首个事件后等 300ms 静默再发一次（合并 append / rename 产生的多个事件）。
        loop {
            if rx.recv().is_err() {
                break;
            }
            loop {
                match rx.recv_timeout(Duration::from_millis(300)) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            // 窗口已关闭 → emit 失败 → 退出线程，自动释放 watcher。
            if window.emit("history-updated", ()).is_err() {
                break;
            }
        }
    });
}

/// 最小化的 URL query 值百分号编码：仅保留 RFC 3986 unreserved 字符（A-Za-z0-9-._~），
/// 其余字节按 UTF-8 逐字节编码为 `%XX`。用于把项目 key / 名称安全地拼进历史窗口 URL。
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// 创建（或聚焦已存在的）Agent 控制台窗口（spec D13 / gui-agent-console）。
/// `session` 为可选目标会话（R4 可寻址打开）：已开窗经 `agents-goto` 事件选中，
/// 新建经 URL 参数传递。`pin_above_popup` 与设置/历史窗口同义，保证从置顶弹窗打开时可见。
pub(crate) fn create_agents_window<R, M>(
    manager: &M,
    config: &AppConfig,
    session: Option<&str>,
    pin_above_popup: bool,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    let session = session.filter(|s| !s.is_empty());
    if let Some(w) = manager.get_webview_window("agents") {
        let _ = w.set_always_on_top(pin_above_popup);
        let _ = w.set_focus();
        if let Some(sid) = session {
            use tauri::Emitter;
            let _ = w.emit("agents-goto", serde_json::json!({ "session": sid }));
        }
        return Ok(());
    }
    let theme = window_theme(config);
    let lang = Lang::resolve(&config.general.language);
    let window_bg = background_for(resolved_theme(config));
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    let mut url = String::from("index.html?view=agents");
    if let Some(sid) = session {
        url.push_str("&session=");
        url.push_str(&urlencode(sid));
    }
    append_window_effect_query(&mut url, effective_window_effect);
    let builder = WebviewWindowBuilder::new(manager, "agents", WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.agents"))
        // 双栏控制台（spec gui-agent-console C1）：边栏 + 详情区需要更宽的默认尺寸。
        .inner_size(980.0, 640.0)
        .min_inner_size(760.0, 480.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let win = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
    Ok(())
}

/// 创建（或聚焦已存在的）项目待办窗口（spec todo-whats-next D9）：全局唯一（label `todos`）。
/// `project_override` 为 Some 时窗口预选该项目（经 URL 参数传递）；None 由前端自选默认项目。
/// 实时同步：监听 `todos.json` 变化 → `todos-updated` 事件（daemon 不参与，窗口独立可用）。
pub(crate) fn create_todos_window<R, M>(
    manager: &M,
    config: &AppConfig,
    project_override: Option<&str>,
    pin_above_popup: bool,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    if let Some(w) = manager.get_webview_window("todos") {
        let _ = w.set_focus();
        // 已开窗时带新预选项目 → 通知前端切换（与设置窗口 goto-tab 同模式）。
        if let Some(key) = project_override {
            use tauri::Emitter;
            let _ = w.emit("todos-goto-project", key.to_string());
        }
        return Ok(());
    }
    let theme = window_theme(config);
    let lang = Lang::resolve(&config.general.language);
    let window_bg = background_for(resolved_theme(config));
    let mut url = String::from("index.html?view=todos");
    if let Some(key) = project_override {
        url.push_str("&project=");
        url.push_str(&urlencode(key));
    }
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    append_window_effect_query(&mut url, effective_window_effect);
    let builder = WebviewWindowBuilder::new(manager, "todos", WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.todos"))
        .inner_size(520.0, 560.0)
        .min_inner_size(400.0, 320.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let win = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
    watch_todos_file(win);
    Ok(())
}

/// 监听 `todos.json` 变更并向目标窗口发 `todos-updated`（待办窗口与提问 Popup 都据此重载；
/// 写入方可能是任意进程，靠文件监听跨进程感知）。原子写（tmp + rename）换 inode，故监听
/// **state 目录**再按文件名过滤（与 `watch_history_file` 同思路）。
fn watch_todos_file<R: tauri::Runtime>(window: tauri::WebviewWindow<R>) {
    use tauri::Emitter;
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        use std::sync::mpsc::{channel, RecvTimeoutError};
        use std::time::Duration;
        let dir = crate::paths::state_dir();
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = channel::<()>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let hit = ev
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n == "todos.json").unwrap_or(false));
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
            // 去抖：合并连续写入事件。
            loop {
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(()) => continue,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            }
            // 窗口已关闭 → emit 失败 → 退出线程，自动释放 watcher。
            if window.emit("todos-updated", ()).is_err() {
                break;
            }
        }
    });
}

/// 创建（或聚焦已存在的）「新建 Agent 任务」窗口（spec gui-agent-task-launch）：全局唯一
/// （label `newtask`）。`project_override` / `todo_override` 为预选项目 key 与待办 id（经 URL
/// 参数传递）；已开窗时经 `newtask-goto` 事件更新预选。`todos.json` 变化经 `todos-updated`
/// 事件驱动前端重载所选项目待办（复用 `watch_todos_file`）。
pub(crate) fn create_new_task_window<R, M>(
    manager: &M,
    config: &AppConfig,
    project_override: Option<&str>,
    todo_override: Option<&str>,
    pin_above_popup: bool,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    if let Some(w) = manager.get_webview_window("newtask") {
        let _ = w.set_focus();
        // 已开窗时带新预选打开 → 通知前端整体重置到新预选（与待办窗口 goto-project 同模式）。
        if project_override.is_some() || todo_override.is_some() {
            use tauri::Emitter;
            let _ = w.emit(
                "newtask-goto",
                serde_json::json!({
                    "project": project_override,
                    "todo": todo_override,
                }),
            );
        }
        return Ok(());
    }
    let theme = window_theme(config);
    let lang = Lang::resolve(&config.general.language);
    let window_bg = background_for(resolved_theme(config));
    let mut url = String::from("index.html?view=newtask");
    if let Some(key) = project_override {
        url.push_str("&project=");
        url.push_str(&urlencode(key));
    }
    if let Some(id) = todo_override {
        url.push_str("&todo=");
        url.push_str(&urlencode(id));
    }
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    append_window_effect_query(&mut url, effective_window_effect);
    let builder = WebviewWindowBuilder::new(manager, "newtask", WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.newTask"))
        .inner_size(520.0, 640.0)
        .min_inner_size(440.0, 520.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let win = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
    watch_todos_file(win);
    Ok(())
}

/// Create or retarget the global native-session Fork window. It is independent from `newtask`, so
/// drafts in either workflow never overwrite the other.
pub(crate) fn create_fork_task_window<R, M>(
    manager: &M,
    config: &AppConfig,
    source_session_id: &str,
    pin_above_popup: bool,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    if let Some(window) = manager.get_webview_window("fork-task") {
        use tauri::Emitter;
        let _ = window.emit(
            "forktask-goto",
            serde_json::json!({ "session": source_session_id }),
        );
        let _ = window.set_focus();
        return Ok(());
    }
    let theme = window_theme(config);
    let window_bg = background_for(resolved_theme(config));
    let mut url = String::from("index.html?view=forktask&session=");
    url.push_str(&urlencode(source_session_id));
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    append_window_effect_query(&mut url, effective_window_effect);
    let lang = Lang::resolve(&config.general.language);
    let builder = WebviewWindowBuilder::new(manager, "fork-task", WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.forkTask"))
        .inner_size(520.0, 560.0)
        .min_inner_size(440.0, 480.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let window = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&window, window_effect, window_bg);
    Ok(())
}

/// 创建（或聚焦已存在的）插话 composer 窗口（spec agent-interject D7）：**每 session 全局唯一**
/// （label 带 session 哈希）。URL 携带 session / agent 家族 / 项目显示名，前端据此渲染头部；
/// 待送达预填文本由前端经 `interject_init` 向 daemon 查询（连接生命周期与窗口一致）。
/// `pin_above_popup` 语义同 [`create_settings_window`]。
pub(crate) fn create_interject_window<R, M>(
    manager: &M,
    config: &AppConfig,
    target: &crate::gui_host::InterjectTarget,
    pin_above_popup: bool,
) -> tauri::Result<()>
where
    R: tauri::Runtime,
    M: Manager<R>,
{
    let label = crate::gui_host::interject_label(&target.session);
    if let Some(w) = manager.get_webview_window(&label) {
        let _ = w.set_focus();
        return Ok(());
    }
    let theme = window_theme(config);
    let lang = Lang::resolve(&config.general.language);
    let window_bg = background_for(resolved_theme(config));
    let mut url = String::from("index.html?view=interject&session=");
    url.push_str(&urlencode(&target.session));
    if let Some(agent) = target.agent.as_deref() {
        url.push_str("&kind=");
        url.push_str(&urlencode(agent));
    }
    if let Some(cwd) = target.cwd.as_deref() {
        // 预算好显示名（目录 basename），前端免再拆路径。
        url.push_str("&project=");
        url.push_str(&urlencode(&crate::project::display_name(cwd)));
    }
    let window_effect = config.general.window_effect;
    let effective_window_effect = effective_window_effect(window_effect);
    append_window_effect_query(&mut url, effective_window_effect);
    let builder = WebviewWindowBuilder::new(manager, &label, WebviewUrl::App(url.into()))
        .title(i18n::tr(lang, "title.interject"))
        .inner_size(520.0, 340.0)
        .min_inner_size(420.0, 260.0)
        .center()
        .always_on_top(pin_above_popup)
        .theme(theme);
    #[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
    let win = apply_surface(builder, window_bg, effective_window_effect).build()?;
    #[cfg(target_os = "macos")]
    set_runtime_window_effect_with_bg(&win, window_effect, window_bg);
    Ok(())
}

/// 由前端在 `agents-updated` 监听就绪后经命令触发，确保 daemon 一连上推来的首帧立即快照不会
/// 早于监听注册而丢失。
///
/// - **统一 GUI 宿主**（长命进程）：订阅与 agent 窗口生命周期绑定——每次前端挂载都**重启**订阅
///   （让 daemon 重推一帧立即快照，避免长命进程里复用旧订阅而首屏长时间 Loading），窗口关闭即停
///   （释放 daemon 连接，不再把 daemon 续命）。详见 `gui_host::restart_agents_subscription`。
/// - **独立 agents 进程 / 弹窗兜底**（随窗口退出的短命进程）：一次性启动即可（进程退出即停）。
pub(crate) fn start_agents_subscription(app: tauri::AppHandle) {
    if app.try_state::<gui_host::HostState>().is_some() {
        gui_host::restart_agents_subscription(&app);
        return;
    }
    static STARTED: AtomicBool = AtomicBool::new(false);
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    spawn_agents_subscription(app, None);
}

/// 控制台焦点槽：`(当前订阅周期的焦点发送端, 最近一次设置的焦点会话)`。
/// 焦点经订阅连接送达 daemon（spec gui-agent-console C8）；断连重连后按 `.1` 补发恢复。
type AgentsFocusSlot = std::sync::Mutex<(
    Option<tokio::sync::mpsc::UnboundedSender<Option<String>>>,
    Option<String>,
)>;

fn agents_focus_slot() -> &'static AgentsFocusSlot {
    static SLOT: std::sync::OnceLock<AgentsFocusSlot> = std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new((None, None)))
}

/// 设置控制台焦点会话（None＝取消）：记录以供重连补发，并 best-effort 发给当前订阅周期。
pub(crate) fn set_agents_focus(session_id: Option<String>) {
    let Ok(mut slot) = agents_focus_slot().lock() else {
        return;
    };
    slot.1 = session_id.clone();
    if let Some(tx) = slot.0.as_ref() {
        let _ = tx.send(session_id);
    }
}

/// 订阅 daemon 的 agent 快照推送，转成前端 `agents-updated` 事件（实验性功能 spec D20）；
/// 焦点会话详情帧转成 `agent-detail` 事件（spec gui-agent-console C8）。
/// 断连后退避重连（必要时 `open_for_subscribe` 会自动拉起 daemon），重连补发当前焦点。
/// `stop` 为 Some 时（宿主）被通知即整体退出（窗口关闭/重启订阅用）；为 None 时随进程退出。
pub(crate) fn spawn_agents_subscription(
    app: tauri::AppHandle,
    stop: Option<std::sync::Arc<tokio::sync::Notify>>,
) {
    use crate::ipc::{self, ClientMsg, ServerMsg};
    use tauri::Emitter;
    tauri::async_runtime::spawn(async move {
        loop {
            // 一轮「连接 → 订阅 → 读到断连」+ 退避；与 stop 竞速，stop 触发即退出整个任务。
            let cycle = async {
                if let Ok((mut reader, mut writer)) = crate::client::open_for_subscribe().await {
                    if ipc::write_msg(&mut writer, &ClientMsg::AgentsSubscribe)
                        .await
                        .is_err()
                    {
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        return;
                    }
                    // 焦点写任务：登记本周期发送端，补发当前焦点（重连恢复），随后按需转发。
                    let (ftx, mut frx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
                    {
                        let Ok(mut slot) = agents_focus_slot().lock() else {
                            return;
                        };
                        if let Some(current) = slot.1.clone() {
                            let _ = ftx.send(Some(current));
                        }
                        slot.0 = Some(ftx);
                    }
                    let writer_task = tokio::spawn(async move {
                        while let Some(session_id) = frx.recv().await {
                            if ipc::write_msg(&mut writer, &ClientMsg::AgentsFocus { session_id })
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    });
                    loop {
                        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
                            Ok(Some(ServerMsg::AgentsState { agents })) => {
                                let _ = app.emit("agents-updated", agents);
                            }
                            Ok(Some(ServerMsg::AgentDetail { detail })) => {
                                let _ = app.emit("agent-detail", detail);
                            }
                            Ok(Some(_)) => {}
                            Ok(None) | Err(_) => break, // 断连 → 跳出去重连。
                        }
                    }
                    writer_task.abort();
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            };
            match &stop {
                Some(s) => {
                    tokio::select! {
                        _ = s.notified() => return,
                        _ = cycle => {}
                    }
                }
                None => cycle.await,
            }
        }
    });
}

/// 原生窗口/webview 底色（与前端 tokens.css `--bg` 对齐）。
fn background_for(theme: tauri::Theme) -> tauri::window::Color {
    match theme {
        tauri::Theme::Dark => tauri::window::Color(30, 30, 30, 255),
        _ => tauri::window::Color(240, 240, 242, 255),
    }
}

pub(crate) fn background_for_theme_name(theme: &str) -> tauri::window::Color {
    let resolved = match theme {
        "light" => tauri::Theme::Light,
        "dark" => tauri::Theme::Dark,
        _ => match dark_light::detect() {
            Ok(dark_light::Mode::Dark) => tauri::Theme::Dark,
            _ => tauri::Theme::Light,
        },
    };
    background_for(resolved)
}

fn window_theme(config: &AppConfig) -> Option<tauri::Theme> {
    match config.general.theme {
        ThemeMode::Light => Some(tauri::Theme::Light),
        ThemeMode::Dark => Some(tauri::Theme::Dark),
        ThemeMode::System => None,
    }
}

/// 记住窗口尺寸：用户拉伸后把逻辑尺寸写回配置。
fn persist_popup_size(window: &tauri::Window) {
    let state = window.app_handle().state::<AppState>();
    if !state.config.channels.popup.remember_size {
        return;
    }
    if let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) {
        // Only the popup size changes; load without secrets so save() neither reads nor rewrites
        // the keychain (blank secret fields are left as-is by save()).
        let mut cfg = AppConfig::load_without_secrets();
        cfg.channels.popup.width = size.width as f64 / scale;
        cfg.channels.popup.height = size.height as f64 / scale;
        let _ = cfg.save();
    }
}

/// GUI 是否可用（进入 Tauri 前的轻量预探测）。
///
/// 因 release 为 `panic = "abort"`，无法用 `catch_unwind` 兜住 GUI 初始化崩溃，
/// 故在 Linux 上先探测显示环境与 WebKitGTK；实际 `build()` 失败仍由调用方按 `Err` 兜底。
/// macOS / Windows 使用系统 WebView，默认视为可用。
#[cfg(target_os = "linux")]
fn gui_available(lang: Lang) -> Result<(), String> {
    let has_display =
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
    if !has_display {
        return Err(i18n::tr(lang, "app.noDisplay").to_string());
    }
    if !webkitgtk_loadable() {
        return Err(i18n::tr(lang, "app.noWebkitgtk").to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn gui_available(_lang: Lang) -> Result<(), String> {
    Ok(())
}

/// 探测 WebKitGTK 运行库是否可被加载（dlopen 成功即视为可用）。
#[cfg(target_os = "linux")]
fn webkitgtk_loadable() -> bool {
    use std::ffi::CString;
    const CANDIDATES: [&str; 4] = [
        "libwebkit2gtk-4.1.so.0",
        "libwebkit2gtk-4.1.so",
        "libwebkit2gtk-4.0.so.37",
        "libwebkit2gtk-4.0.so",
    ];
    for name in CANDIDATES {
        if let Ok(c) = CString::new(name) {
            unsafe {
                let handle = libc::dlopen(c.as_ptr(), libc::RTLD_LAZY);
                if !handle.is_null() {
                    libc::dlclose(handle);
                    return true;
                }
            }
        }
    }
    false
}

/// 静默 GUI 事件循环期间的系统噪音日志：把进程 stderr 重定向到 /dev/null，
/// 同时保存原始 stderr 句柄，供我们自己的错误信息照常输出。
#[cfg(unix)]
mod stderr_redirect {
    use std::sync::atomic::{AtomicI32, Ordering};

    static SAVED: AtomicI32 = AtomicI32::new(-1);

    pub fn silence() {
        unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return;
            }
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY);
            if devnull < 0 {
                libc::close(saved);
                return;
            }
            libc::dup2(devnull, libc::STDERR_FILENO);
            libc::close(devnull);
            SAVED.store(saved, Ordering::SeqCst);
        }
    }

    pub fn eprintln_real(msg: &str) {
        let fd = SAVED.load(Ordering::SeqCst);
        let line = format!("{}\n", msg);
        if fd >= 0 {
            unsafe {
                libc::write(fd, line.as_ptr() as *const libc::c_void, line.len());
            }
        } else {
            eprint!("{}", line);
        }
    }
}

#[cfg(not(unix))]
mod stderr_redirect {
    pub fn silence() {}
    pub fn eprintln_real(msg: &str) {
        eprintln!("{}", msg);
    }
}
