//! Daemon 主体（Unix）：状态与类型、serve 主循环、连接分发、请求提交与生命周期命令。
//! watch/select/inbound/subs/detect 的自由函数拆为子模块，经 glob 导入保持单一命名空间。

use super::ask_dedup;
use super::config_watch;
use super::lifecycle::{self, DaemonMeta, LockGuard};
use super::popup_focus::{PopupEffect, PopupFocusArbiter, ReadyMetadata};
use super::request::{self, InteractionEntry, RequestEntry, RequestRegistry};
use crate::agents::registry::AgentRegistry;
use crate::agents::{AgentKind, LifecycleEvent};
use crate::app::confirm_coordinator::ConfirmOutcome;
use crate::channels::dingding::DingTalkChannel;
use crate::channels::feishu::FeishuChannel;
use crate::channels::slack::SlackChannel;
use crate::channels::telegram::TelegramChannel;
use crate::channels::Channel;
use crate::client;
use crate::config::AppConfig;
use crate::dingtalk::router::DdRouter;
use crate::feishu::router::FsRouter;
use crate::i18n::Lang;
use crate::ipc::{
    self, transport, ClientMsg, ConfirmTask, DetectRequest, HelloAck, HelloStatus,
    PopupPresentation, ServerMsg, StatusInfo, TaskRequest,
};
use crate::models::{ChannelAction, ChannelResult, ConfirmFallbackReason};
use crate::slack::router::SlRouter;
use crate::telegram::router::TgRouter;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::io::BufReader;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::UnixStream;

mod detect;
mod inbound;
mod select;
mod subs;
mod todo;
mod watch;

use detect::*;
use inbound::*;
use select::*;
use subs::*;
use todo::*;
use watch::*;

type Reader = BufReader<OwnedReadHalf>;

/// 无活动且无连接持续此时长后自动退出。可用 `ASKHUMAN_DAEMON_IDLE_SECS` 覆盖（便于测试）。
const DEFAULT_IDLE_SECS: u64 = 300;

fn idle_timeout() -> Duration {
    let secs = std::env::var("ASKHUMAN_DAEMON_IDLE_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_IDLE_SECS);
    Duration::from_secs(secs)
}

/// 「工作中」兜底超时秒数。默认 `WORKING_BACKSTOP_SECS`（30min），
/// 可用 `ASKHUMAN_WORKING_BACKSTOP_SECS` 覆盖（便于实测自愈，无需真等 30 分钟）。
fn working_backstop_secs() -> u64 {
    std::env::var("ASKHUMAN_WORKING_BACKSTOP_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&s| s > 0)
        .unwrap_or(crate::agents::registry::WORKING_BACKSTOP_SECS)
}

fn version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn log(msg: &str) {
    // `daemon run` 经 spawn 时 stderr 已重定向到 daemon.log；前台运行则打到终端。
    eprintln!("[askhuman-daemon {}] {}", now_secs(), msg);
}

fn block_on<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build current-thread runtime")
        .block_on(f)
}

pub fn dispatch(args: &[String]) -> i32 {
    let force = args.iter().skip(1).any(|a| a == "--force");
    // 子命令后仅允许 `--force`（且只对 stop/restart 有意义），其余一律报错。
    if let Some(extra) = args.iter().skip(1).find(|a| a.as_str() != "--force") {
        eprintln!("unknown daemon argument: {}", extra);
        return 1;
    }
    match args.first().map(|s| s.as_str()).unwrap_or("") {
        "run" => run_cmd(),
        "start" => start_cmd(),
        "stop" => stop_cmd(force),
        "restart" => restart_cmd(force),
        "status" => status_cmd(),
        "logs" => logs_cmd(),
        "" => {
            eprintln!(
                "usage: AskHuman daemon <run|start|stop [--force]|restart [--force]|status|logs>"
            );
            1
        }
        other => {
            eprintln!("unknown daemon subcommand: {}", other);
            eprintln!("usage: AskHuman daemon <run|start|stop|restart|status|logs>");
            1
        }
    }
}

// —— run：前台运行 Daemon 主体 ——

fn run_cmd() -> i32 {
    let lock = match lifecycle::acquire_lock() {
        Ok(Some(l)) => l,
        Ok(None) => {
            log("another daemon is already running; exiting");
            return 0;
        }
        Err(e) => {
            log(&format!("failed to acquire lock: {}", e));
            return 1;
        }
    };
    // 复用 tauri 的全局（多线程）tokio 运行时，使各 Channel 的 `tauri::async_runtime::spawn`
    // 与 Daemon 自身的任务跑在同一运行时上（不初始化任何 GUI / AppKit）。
    tauri::async_runtime::block_on(serve(lock))
}

struct ServerState {
    startup_fp: lifecycle::Fingerprint,
    started_at: u64,
    /// 当前打开的连接数（CLI 提交连接 / GUI 连接 / 控制连接）。用于空闲退出判定：
    /// 提交连接在请求存续期间保持打开，故人在环里的长等待算「活动」，不会被空闲计时杀掉。
    active: AtomicUsize,
    last_active: Mutex<Instant>,
    shutdown: tokio::sync::Notify,
    /// 排空状态（graceful drain）：true 时拒绝新 Submit/Detect，在途请求服务到完结后退出。
    draining: AtomicBool,
    /// 活动请求登记表。
    registry: Arc<RequestRegistry>,
    /// Cross-process popup focus owner and background cascade order.
    popup_focus: Mutex<PopupFocusArbiter>,
    /// 钉钉长连接 Router（惰性建连、常热复用；连接死亡后按需重连）。
    dd_router: tokio::sync::Mutex<Option<Arc<DdRouter>>>,
    /// 飞书长连接 Router（惰性建连、常热复用；连接死亡后按需重连）。
    fs_router: tokio::sync::Mutex<Option<Arc<FsRouter>>>,
    /// Telegram 长轮询 Router（惰性建连、常热复用；单一 offset）。
    tg_router: tokio::sync::Mutex<Option<Arc<TgRouter>>>,
    /// Slack Socket Mode Router（惰性建连、常热复用；连接死亡后按需重连）。
    sl_router: tokio::sync::Mutex<Option<Arc<SlRouter>>>,
    /// 最近一次已知配置快照（config watch 据此比对差异，决定哪些 Router 需失效，A12）。
    config: Mutex<AppConfig>,
    /// 版本自更新快照（后台检查 / 指纹监听维护，握手与广播据此告知弹窗）。
    update: Mutex<UpdateSnapshot>,
    /// Agent 生命周期注册表（实验性功能，spec D3）。
    agents: Arc<AgentRegistry>,
    /// Agent 插话队列（spec agent-interject）：内存常驻，变更时落盘 `interject.json`。
    interject: crate::agents::interject::InterjectStore,
    /// Short-lived Grok MCP session-binding candidates and process/instance partitions.
    grok_bindings: crate::context_binding::GrokBindingRegistry,
    /// 状态窗口订阅者的发送端列表（变化 / 心跳时推 `AgentsState`）。
    agent_subs: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<ServerMsg>>>,
    /// 菜单栏宿主订阅者的发送端列表（变化 / 心跳时推 `TrayState`）。
    /// **非保活**：该列表不参与空闲退出判定（见 `handle_tray_sub`），图标不得续命 daemon。
    tray_subs: Mutex<Vec<tokio::sync::mpsc::UnboundedSender<ServerMsg>>>,
    /// 「IM 会话期自动激活」当前活跃槽（持久化、跨重启保留，仅由入站消息改变）。
    active_channel: Mutex<Option<String>>,
    /// 已启动入站监听器的注册表（防重复 spawn + 改配置时主动停旧监听并重建）。
    inbound_listeners: InboundRegistry,
    /// 方案6 弹窗预热「热池」：最多 1 个已挂载、隐藏待命的热实例连接。来请求时 `dispatch_popup` 取出
    /// 并把请求 entry 交给其 holder 任务领用上屏，省掉冷 spawn + WebView 初始化。**非保活**（不计入
    /// `active`），daemon 仍可正常空闲退出（在途请求由 CLI submit 连接保活）。
    warm_pool: Mutex<Option<WarmSlot>>,
    /// 正在补热中（去重，避免并发 spawn 多个热实例）。
    warm_spawning: AtomicBool,
    /// `/watch` 实时关注子系统（spec docs/specs/im-watch.md；飞书/Telegram/Slack）。
    watch: WatchState,
    /// 通用「单选卡」子系统（spec docs/specs/im-select-card.md；MVP 仅飞书）。
    select: SelectState,
    /// IM-created launches waiting to be associated with their lifecycle session.
    pending_launches: Mutex<Vec<PendingLaunchWatch>>,
    /// 控制台焦点会话表（spec gui-agent-console C8）：每个 agents 订阅者至多一条；
    /// watch 引擎按签名推 `AgentDetail` 帧。随订阅连接断开清理，不持久化。
    gui_focus: Mutex<Vec<GuiFocusEntry>>,
    /// 上次广播时的「等待回答」会话集（排序去重）：变化才重推 AgentsState，
    /// 使 `waitingRequestId` 徽标在提问创建/完结时即时刷新（watch 引擎 tick 内比对）。
    last_waiting: Mutex<Vec<String>>,
    /// 每会话最近一条已完成问答（spec duplicate-ask-coalescing D5）：agent 被中断后原样重发
    /// 同一个提问时直接回放上次答案，不再打扰人。仅内存，daemon 换新即失效。
    replay: Mutex<crate::daemon::ask_dedup::ReplayCache>,
}

/// 一条控制台焦点订阅（`tx` 即该 agents 订阅者的发送端；同一订阅者重设焦点就地更新）。
struct GuiFocusEntry {
    tx: tokio::sync::mpsc::UnboundedSender<ServerMsg>,
    session_id: String,
    /// 上一帧签名（内容不变不推）。
    last_sig: String,
    /// 上一帧是否工作中（引擎自适应 tick 用）。
    working: bool,
}

impl ServerState {
    /// 当前生效配置的快照。启动时 `load()` 初始化、config_watch 变更后刷新（`on_config_changed`），
    /// 密钥已解析。热路径（watch tick、入站消息、卡片回调、提交投放）一律读它，避免每次
    /// 「读盘 + JSON 解析 + macOS 钥匙串 IPC」；代价是配置变更后有 config_watch 去抖窗（~300ms）
    /// 的短暂陈旧，下一次读取即新。
    fn config_snapshot(&self) -> AppConfig {
        self.config.lock().unwrap().clone()
    }
}

fn apply_popup_effects(state: &Arc<ServerState>, effects: Vec<PopupEffect>) {
    for effect in effects {
        let (request_id, message) = match effect {
            PopupEffect::PresentForeground { request_id } => {
                let message = ServerMsg::PresentPopup {
                    request_id: request_id.clone(),
                    presentation: PopupPresentation::Foreground,
                };
                (request_id, message)
            }
            PopupEffect::PresentBackground {
                request_id,
                cascade_index,
                behind_window_number,
            } => {
                let message = ServerMsg::PresentPopup {
                    request_id: request_id.clone(),
                    presentation: PopupPresentation::BackgroundCascade {
                        cascade_index,
                        behind_window_number,
                    },
                };
                (request_id, message)
            }
            PopupEffect::Focus { request_id } => {
                let message = ServerMsg::FocusPopup {
                    request_id: request_id.clone(),
                };
                (request_id, message)
            }
        };
        let _ = state.registry.send_to_gui(&request_id, message);
    }
}

fn update_popup_focus(
    state: &Arc<ServerState>,
    update: impl FnOnce(&mut PopupFocusArbiter) -> Vec<PopupEffect>,
) {
    let effects = {
        let mut focus = state.popup_focus.lock().unwrap();
        update(&mut focus)
    };
    apply_popup_effects(state, effects);
}

#[derive(Clone)]
struct PendingLaunchWatch {
    id: String,
    channel: String,
    kind: AgentKind,
    cwd: String,
    task_sha256: String,
    created_at: u64,
}

fn pending_launch_matches(
    item: &PendingLaunchWatch,
    kind: AgentKind,
    cwd: Option<&str>,
    launch_id: Option<&str>,
    prompt_sha256: Option<&str>,
) -> bool {
    item.kind == kind
        && cwd.is_some_and(|value| value == item.cwd)
        && (launch_id.is_some_and(|value| value == item.id)
            || prompt_sha256.is_some_and(|value| value == item.task_sha256))
}

/// 「跟底」重发节流：同一订阅两次跟底之间的最短间隔（用户定案 30s）。
const WATCH_MOVE_THROTTLE_MS: u64 = 30_000;

/// MCP clears Agent environment markers, so IM delivery briefly waits for daemon's already-running
/// process-tree resolution. Popup remains non-blocking and receives its existing async update.
const IM_AGENT_RESOLVE_WAIT_MS: u64 = 200;

/// rewatchable entry 保留时限（秒）：超时后自动清理（路由失效、按钮不再可用）。
const REWATCHABLE_TTL_SECS: u64 = 600;

/// `/watch` 实时关注子系统的 daemon 侧状态。
#[derive(Default)]
struct WatchState {
    /// 活动订阅（agent 结束 / 空闲宽限期到期 / 用户取消即移除；每渠道上限 `watch::MAX_WATCHES`）。
    subs: Mutex<Vec<WatchEntry>>,
    /// 引擎唤醒信号（AgentEvent / 提问创建、完结 / 订阅变化）。
    notify: tokio::sync::Notify,
    /// 渠道 id → 卡片按钮回调路由任务句柄（随 Router 生命周期 / 订阅集合变化整体重建）。
    routes: Mutex<HashMap<String, WatchRouteHandle>>,
    /// 渠道 id → 「最后一条非 watch 消息」时刻（Unix 毫秒）——跟底判定的**淹没信号**。
    /// 只有非 watch 消息才算淹没：watch 卡之间互不影响（用户定案）。
    disturb: Mutex<HashMap<String, u64>>,
    /// 渠道 id → 缓存的传输客户端（连接池跨拍复用；Slack 免每拍 `open_dm`）。
    /// 配置变更时整体失效（见 `on_config_changed`）。
    clients: Mutex<HashMap<String, Arc<WatchClient>>>,
}

/// 一条活动的 watch 订阅（引擎工作台账；持久化时压成 `watch::PersistedWatch`）。
#[derive(Clone)]
struct WatchEntry {
    /// 卡片所在渠道 id（feishu / telegram / slack）。
    channel: String,
    /// 被关注 agent 的 session_id（身份键）。
    session_id: String,
    /// 实时状态卡的消息 id（编辑目标；跟底重发后换新）。渠道各异：飞书 open_message_id、
    /// Telegram message_id 十进制串、Slack 消息 ts、钉钉 outTrackId（自铸 uuid）。
    message_id: String,
    /// 展示编号（同 `/status`；daemon 生命周期内稳定，重启恢复时按 session 重解析）。
    seq: u64,
    created_at: u64,
    /// 上一帧签名：内容不变不编辑（防无谓编辑请求）。
    last_sig: String,
    /// 上次成功编辑时刻（Unix 毫秒）：每卡最短编辑间隔按渠道（`WatchClient::min_edit_interval_ms`）。
    last_edit_ms: u64,
    /// 连续编辑失败次数（≥5 自动退订：超时不可改 / 卡被删等）。
    fails: u32,
    /// 上一帧是否「工作中」（引擎自适应 tick：有工作中 2s，否则 10s；Idle 宽限期走后者）。
    working: bool,
    /// 当前卡发出时刻（Unix 毫秒）：与渠道 disturb 水位比较判定卡是否已被淹没。
    sent_at_ms: u64,
    /// 上次跟底重发时刻（Unix 毫秒）：30s 节流；提问答复完结时清零（下次更新立即跟底）。
    last_move_ms: u64,
    /// 终态已定格但保留路由供重新关注（仅 AutoStopped；引擎/上限/空闲退出跳过此类 entry）。
    rewatchable: bool,
}

/// watch 卡片回调路由任务的句柄：绑定特定 Router 实例与注册的卡片集合，
/// 任一变化（Router 重建 / 订阅增减）都停旧任务整体重建。
struct WatchRouteHandle {
    stop: Arc<tokio::sync::Notify>,
    router: WatchRouterRef,
    /// 任务注册路由时的卡片 message_id 集合（已排序，用于变更比较）。
    mids: Vec<String>,
}

/// 路由任务绑定的 Router 弱引用（按渠道类型分列，用于「同一存活 Router」比对）。
enum WatchRouterRef {
    Feishu(std::sync::Weak<FsRouter>),
    Telegram(std::sync::Weak<TgRouter>),
    Slack(std::sync::Weak<SlRouter>),
    DingTalk(std::sync::Weak<DdRouter>),
}

impl WatchRouterRef {
    /// 是否仍绑定同一个存活的 Router 实例。
    fn is_same_alive(&self, ch: &WatchChannelRouter) -> bool {
        match (self, ch) {
            (WatchRouterRef::Feishu(w), WatchChannelRouter::Feishu(r)) => w
                .upgrade()
                .map(|x| Arc::ptr_eq(&x, r) && x.is_alive())
                .unwrap_or(false),
            (WatchRouterRef::Telegram(w), WatchChannelRouter::Telegram(r)) => w
                .upgrade()
                .map(|x| Arc::ptr_eq(&x, r) && x.is_alive())
                .unwrap_or(false),
            (WatchRouterRef::Slack(w), WatchChannelRouter::Slack(r)) => w
                .upgrade()
                .map(|x| Arc::ptr_eq(&x, r) && x.is_alive())
                .unwrap_or(false),
            (WatchRouterRef::DingTalk(w), WatchChannelRouter::DingTalk(r)) => w
                .upgrade()
                .map(|x| Arc::ptr_eq(&x, r) && x.is_alive())
                .unwrap_or(false),
            _ => false,
        }
    }
}

/// 某渠道当前的共享 Router（强引用，路由重建时用）。
enum WatchChannelRouter {
    Feishu(Arc<FsRouter>),
    Telegram(Arc<TgRouter>),
    Slack(Arc<SlRouter>),
    DingTalk(Arc<DdRouter>),
}

/// 通用「单选卡」子系统的 daemon 侧状态（spec docs/specs/im-select-card.md）。
/// 普通 picker 不持久化；`MsgCompose` 仅持久化不含正文的最小恢复记录，以便重启后定格旧输入卡。
/// 同台账挂 `/stage` Confirm 卡（spec im-diff-stage-transcript；不持久化，TTL 同 picker）。
struct SelectState {
    /// 活动的单选卡台账（被消费即移除；软上限 + TTL 兜底清理，见 `register_picker`）。
    pickers: Mutex<Vec<PickerEntry>>,
    /// `/stage` 确认卡台账（与 pickers 共享路由重建，message_id 并入 ensure_select_routes）。
    confirms: Mutex<Vec<ConfirmEntry>>,
    /// 渠道 id → 卡片按钮回调路由任务句柄（复用 watch 的 `WatchRouteHandle`）。
    routes: Mutex<HashMap<String, WatchRouteHandle>>,
    /// 异步请求重建 select/confirm 路由（打破 ensure_select_route_for ↔ card handler 的 async 环）。
    route_refresh: tokio::sync::Notify,
    /// 已消费 `/msg` 输入卡的短期 tombstone（key=`channel\nmessage_id`，value=过期秒）。
    /// 主要封住 Telegram 原始观察者与 ForceReply 路由并发消费时的重复入站窗口。
    msg_compose_tombstones: Mutex<HashMap<String, u64>>,
}

impl Default for SelectState {
    fn default() -> Self {
        let now = now_secs();
        let pickers: Vec<PickerEntry> = crate::msg_card::load_recovery()
            .into_iter()
            .filter_map(|item| {
                let payload =
                    crate::msg_card::encode_payload(&crate::msg_card::MsgComposePayload {
                        session_id: item.session_id,
                        expires_at: item.expires_at,
                        recovered: true,
                    })?;
                Some(PickerEntry {
                    channel: item.channel,
                    message_id: item.message_id,
                    kind: PickerKind::MsgCompose,
                    title: crate::i18n::tr(Lang::current(), "msgCard.recoveredTitle").to_string(),
                    options: Vec::new(),
                    payload: Some(payload),
                    created_at: now,
                    posted_ms: 0,
                })
            })
            .collect();
        Self {
            pickers: Mutex::new(pickers),
            confirms: Mutex::new(Vec::new()),
            routes: Mutex::new(HashMap::new()),
            route_refresh: tokio::sync::Notify::new(),
            msg_compose_tombstones: Mutex::new(HashMap::new()),
        }
    }
}

/// 单选卡种类（决定点选后做什么；接新命令只需加一档 + 其选中动作）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PickerKind {
    TaskWorkspace,
    TaskAgent,
    TaskPermission,
    TaskInputSource,
    Watch,
    Status,
    Unwatch,
    /// 发送插话（`/msg` 无编号）：点选把 `PickerEntry::payload` 发给该 agent。
    Msg,
    /// `/msg` 一次性输入卡：`payload` 是 `MsgComposePayload`，选项恒空。
    MsgCompose,
    Diff,
    Stage,
    Transcript,
    /// `/todo` 的选项目卡：点选后发项目待办管理卡，或把 `payload` 文本加入项目。
    Todo,
    /// `/todo-rm` 的选项目卡：点选后本卡就地变身为该项目的逐条删除卡。
    TodoRm,
    /// 待办逐条删除卡：`options`＝待办条目 id，`payload`＝项目 key；点「删除」即移除并就地刷新。
    TodoRmEntry,
    /// `/todo-auto` 的选项目卡：点选后变身为切换卡，或把 `payload` 文本加入项目并标为自动执行。
    TodoAuto,
    /// 待办自动执行切换卡：`options`＝待办条目 id，`payload`＝项目 key；点「切换」即开/关并就地刷新。
    TodoAutoEntry,
    /// 待办管理卡（飞书代码卡 / 钉钉提问卡模板）：无行按钮，仅表单「新增」提交；`payload`＝项目 key。
    TodoManage,
    /// `/yolo` 的会话选择卡（D53）：选项＝开着 YOLO 的 Codex session id，点「关闭」即关。
    Yolo,
}

/// `/stage` 确认卡台账（不持久化）。业务状态留在此处；通用 view 只负责展示与动作槽映射。
#[derive(Clone)]
struct ConfirmEntry {
    channel: String,
    message_id: String,
    session_id: String,
    git_root: std::path::PathBuf,
    paths_fp: String,
    /// 通用展示模型；wire slot 必须经它映射回 `/stage` 的稳定业务 action id。
    view: crate::confirm::ConfirmView,
    created_at: u64,
}

/// 一条活动的单选卡台账。选项快照仅存各选项的稳定 id（下标即按钮 idx），点击时按下标取 id。
#[derive(Clone)]
struct PickerEntry {
    channel: String,
    message_id: String,
    kind: PickerKind,
    /// 卡片当前标题快照（变身时随卡更新）：关停定格「已失效」终态卡时复用（第 15 轮定案）。
    title: String,
    /// 各选项的稳定 id（下标 = 按钮 `select:<idx>`）：agent 卡＝session_id；项目卡＝项目 key；
    /// `TodoRmEntry` / `TodoAutoEntry` 卡＝待办条目 id。
    options: Vec<String>,
    /// `Msg` / `Todo` / `TodoAuto` 卡＝待发送内容；`MsgCompose`＝目标与过期时间 JSON；
    /// `TodoRmEntry` / `TodoAutoEntry` 卡＝项目 key；`TodoManage` 卡＝含项目 key 的 JSON；其它恒 `None`。
    payload: Option<String>,
    created_at: u64,
    /// 发卡时刻的渠道扰动水位（Unix 毫秒，同 `WatchState::disturb` 量纲）：与当前渠道水位比较判定
    /// 本单选卡是否仍位于会话底部（其后未再出现非 watch 消息）。用于「仅当单选卡还是最后一条
    /// 消息时才抑制 watch 跟底」（见 `select_is_last_on`）。
    posted_ms: u64,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskPickerPayload {
    #[serde(default)]
    workspace: String,
    #[serde(default)]
    kind: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskInputSourcePayload {
    task: TaskPickerPayload,
    permission: crate::integrations::agent_launch::LaunchPermission,
    project: String,
    todos: Vec<crate::todos::TodoEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    overflow_note: Option<String>,
}

const TASK_MANUAL_ACTION_ID: &str = "start";
const TASK_TODO_ACTION_PREFIX: &str = "todo:";

fn task_todo_label(todo: &crate::todos::TodoEntry, lang: Lang) -> String {
    format!(
        "{}{}{}",
        crate::i18n::tr(lang, "whatsNext.todoPrefix"),
        if todo.auto { "⚡ " } else { "" },
        todo.text
    )
}

/// 单选卡台账治理上限：每渠道最多留存的活动单选卡数（超出丢最旧）。
const SELECT_MAX_PICKERS_PER_CHANNEL: usize = 20;
/// 单选卡台账 TTL（秒）：超龄未消费即清理（兜底，避免长期累积）。
const SELECT_PICKER_TTL_SECS: u64 = 1800;

/// 热池中一个待命热实例的句柄：`assign` 用于把领用的请求 entry 交给其 holder 任务（`handle_gui_warm`）；
/// `gui_tx` 仅用于身份比对（热进程自然死亡时判定池中是否仍是本槽）。
struct WarmSlot {
    assign: tokio::sync::oneshot::Sender<InteractionEntry>,
    gui_tx: tokio::sync::mpsc::UnboundedSender<ServerMsg>,
}

/// 入站监听注册表：渠道 id → 该监听任务的停止信号（`Notify`）。
///
/// 既防重复 spawn（已认领的渠道不再起第二个监听），又支持「改配置时主动停掉旧监听」：
/// `take` 出 stop 后 `notify` 即让旧任务退出、同时立刻释放认领，便于按新连接重建。
/// **释放按身份（`Arc::ptr_eq`）判定**：旧任务退出时只移除「自己那次」的认领，绝不误删
/// 配置变更后新建监听的认领（否则会出现两个监听并存）。
#[derive(Default)]
struct InboundRegistry {
    inner: Mutex<HashMap<String, Arc<tokio::sync::Notify>>>,
}

impl InboundRegistry {
    /// 认领某渠道的监听位：未被认领则插入新 stop 信号并返回；已被认领返回 None。
    fn claim(&self, id: &str) -> Option<Arc<tokio::sync::Notify>> {
        let mut map = self.inner.lock().unwrap();
        if map.contains_key(id) {
            return None;
        }
        let stop = Arc::new(tokio::sync::Notify::new());
        map.insert(id.to_string(), stop.clone());
        Some(stop)
    }

    /// 取出并移除某渠道的 stop 信号（改配置时用：拿到后 notify 停旧监听、同时释放认领）。
    fn take(&self, id: &str) -> Option<Arc<tokio::sync::Notify>> {
        self.inner.lock().unwrap().remove(id)
    }

    /// 身份安全释放：仅当当前登记的 stop 与 `token` 是同一实例（`Arc::ptr_eq`）时才移除。
    /// 监听任务退出时调——避免「改配置后旧任务迟到释放」误删新监听的认领。
    fn release(&self, id: &str, token: &Arc<tokio::sync::Notify>) {
        let mut map = self.inner.lock().unwrap();
        if map.get(id).map(|s| Arc::ptr_eq(s, token)).unwrap_or(false) {
            map.remove(id);
        }
    }
}

/// Daemon 维护的「自更新」当前态（广播给弹窗用）。
#[derive(Clone, Default)]
struct UpdateSnapshot {
    /// 远端有更新且未被忽略。
    available: bool,
    /// 最新正式版版本号。
    latest_version: String,
    /// 新二进制已落盘、待 drain 换新生效。
    pending: bool,
}

/// 启动时从 `update.json` 还原快照：available 据本地版本重算；
/// pending 一律清零——刚启动的 daemon 运行的就是盘上二进制，不存在「更新的二进制在等生效」，
/// 若残留 pending（上次换新前由旧 daemon 落盘）会让弹窗错误地常驻「待生效」横条。
fn init_update_snapshot() -> UpdateSnapshot {
    let st = crate::update::state::load();
    if st.pending {
        crate::update::state::set_pending(false);
    }
    let mut snapshot = update_snapshot_from_state(&st);
    snapshot.pending = false;
    snapshot
}

fn update_snapshot_from_state(st: &crate::update::state::UpdateState) -> UpdateSnapshot {
    let available = !st.latest_version.is_empty()
        && crate::update::compare_versions(&st.latest_version, &crate::update::current_version())
            > 0
        && !st
            .dismissed_versions
            .iter()
            .any(|version| version == &st.latest_version);
    UpdateSnapshot {
        available,
        latest_version: st.latest_version.clone(),
        pending: st.pending,
    }
}

/// 由当前快照构造广播消息。
fn update_state_msg(state: &ServerState) -> ServerMsg {
    let u = state.update.lock().unwrap().clone();
    ServerMsg::UpdateState {
        available: u.available,
        latest_version: u.latest_version,
        pending: u.pending,
    }
}

/// 后台检查一次更新：查远端 → 落 `update.json` → 更新快照 → 有变化则广播。失败静默。
async fn check_for_update(state: &Arc<ServerState>) {
    match crate::update::check().await {
        Ok(info) => {
            let info = crate::update::persist_check_result(info, false);
            let dismissed = crate::update::state::is_dismissed(&info.latest_version);
            let available = info.available && !dismissed;
            let changed = {
                let mut u = state.update.lock().unwrap();
                let changed = u.available != available || u.latest_version != info.latest_version;
                u.available = available;
                u.latest_version = info.latest_version.clone();
                changed
            };
            if changed {
                state.registry.broadcast_to_guis(update_state_msg(state));
                broadcast_tray_state(state);
            }
        }
        Err(e) => log(&format!("update check failed: {}", e)),
    }
}

/// Reload a successful manual check written by Settings or the GUI Host, then immediately update
/// all tray subscribers and open popup helpers.
fn refresh_update_snapshot(state: &Arc<ServerState>) {
    let next = update_snapshot_from_state(&crate::update::state::load());
    let changed = {
        let mut current = state.update.lock().unwrap();
        let changed = current.available != next.available
            || current.latest_version != next.latest_version
            || current.pending != next.pending;
        *current = next;
        changed
    };
    if changed {
        state.registry.broadcast_to_guis(update_state_msg(state));
        broadcast_tray_state(state);
    }
}

/// 检测盘上二进制是否已被换新（应用内更新或外部 npm 更新）。变化则标记 pending + 广播。
fn check_pending_update(state: &Arc<ServerState>) {
    let now_fp = lifecycle::current_fingerprint();
    if now_fp == state.startup_fp {
        return;
    }
    let changed = {
        let mut u = state.update.lock().unwrap();
        let was = u.pending;
        u.pending = true;
        !was
    };
    if changed {
        crate::update::state::set_pending(true);
        log("binary on disk changed; marking update pending");
        state.registry.broadcast_to_guis(update_state_msg(state));
        broadcast_tray_state(state);
        // 主动换新（与 Hello 分支同一套语义，spec self-update）：盘上二进制已变即触发排空换新，
        // 不再被动等下一次握手。长连接（状态窗口订阅 / 工作中 agent）只保活、自身不再发 Hello，
        // 若无人握手旧 daemon 会一直停在旧二进制——这里主动 begin_drain 补上：有在途 ASK 则排空到
        // 完结再退（不打断），无在途立即退；退出前 persist 的 agent 注册表由新 daemon load 复核恢复。
        // 受 ASKHUMAN_DAEMON_AUTORESTART 开关控制（默认开），与 Hello 分支保持一致。
        let auto_restart = std::env::var("ASKHUMAN_DAEMON_AUTORESTART")
            .map(|v| v != "0")
            .unwrap_or(true);
        if auto_restart {
            log("binary on disk changed; draining for restart");
            begin_drain(state);
        }
    }
}

async fn serve(_lock: LockGuard) -> i32 {
    let listener = match transport::bind() {
        Ok(l) => l,
        Err(e) => {
            log(&format!("failed to bind socket: {}", e));
            return 1;
        }
    };
    let startup_fp = lifecycle::current_fingerprint();
    let started_at = now_secs();
    let meta = DaemonMeta {
        pid: std::process::id(),
        version: version(),
        protocol_version: ipc::PROTOCOL_VERSION,
        started_at,
        socket: transport::socket_path().display().to_string(),
        fingerprint: startup_fp,
    };
    if let Err(e) = lifecycle::write_meta(&meta) {
        log(&format!("failed to write daemon.json: {}", e));
    }
    log(&format!(
        "started pid={} version={} protocol={}",
        meta.pid, meta.version, meta.protocol_version
    ));

    // Ensure the user hooks directory exists and includes a sample script.
    crate::hooks::ensure_sample();

    // 自动迁移：已开启生命周期追踪的家族若 hook 过期（升级新增了事件 / 命令路径变化），
    // 启动时幂等重装一次，让已安装用户无需手动关开开关即可拿到新 hook（仅刷新已安装的家族）。
    {
        let migrated = crate::integrations::agent_lifecycle::migrate_outdated();
        if !migrated.is_empty() {
            let names: Vec<&str> = migrated.iter().map(|k| k.as_str()).collect();
            log(&format!(
                "migrated outdated lifecycle hooks: {}",
                names.join(", ")
            ));
        }
        let migrated = crate::integrations::agent_stop::migrate_outdated();
        if !migrated.is_empty() {
            let names: Vec<&str> = migrated.iter().map(|kind| kind.as_str()).collect();
            log(&format!(
                "migrated outdated Stop confirmation hooks: {}",
                names.join(", ")
            ));
        }
    }

    // 保活模式：让 daemon 登录项（下次登录自启）与配置一致（幂等，纯文件；exe 路径变化会刷新）。
    sync_daemon_login_item();
    crate::integrations::agent_launch::cleanup_expired_records();

    let state = Arc::new(ServerState {
        startup_fp,
        started_at,
        active: AtomicUsize::new(0),
        last_active: Mutex::new(Instant::now()),
        shutdown: tokio::sync::Notify::new(),
        draining: AtomicBool::new(false),
        registry: RequestRegistry::new(),
        popup_focus: Mutex::new(PopupFocusArbiter::new()),
        dd_router: tokio::sync::Mutex::new(None),
        fs_router: tokio::sync::Mutex::new(None),
        tg_router: tokio::sync::Mutex::new(None),
        sl_router: tokio::sync::Mutex::new(None),
        config: Mutex::new(AppConfig::load()),
        update: Mutex::new(init_update_snapshot()),
        agents: Arc::new(AgentRegistry::load()),
        interject: crate::agents::interject::InterjectStore::load(),
        grok_bindings: crate::context_binding::GrokBindingRegistry::default(),
        agent_subs: Mutex::new(Vec::new()),
        tray_subs: Mutex::new(Vec::new()),
        active_channel: Mutex::new(crate::autochannel::load_active()),
        inbound_listeners: InboundRegistry::default(),
        warm_pool: Mutex::new(None),
        warm_spawning: AtomicBool::new(false),
        watch: WatchState::default(),
        select: SelectState::default(),
        pending_launches: Mutex::new(Vec::new()),
        gui_focus: Mutex::new(Vec::new()),
        last_waiting: Mutex::new(Vec::new()),
        replay: Mutex::new(crate::daemon::ask_dedup::ReplayCache::new()),
    });

    // 空闲退出检查。
    {
        let state = state.clone();
        tokio::spawn(async move {
            let timeout = idle_timeout();
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                // 保活模式（general.daemonLifecycle = keepalive）：永不因空闲退出（让 IM 随时可收）。
                // 每轮读一次配置（load_without_secrets：仅读 config.json，不碰钥匙串，开销可忽略）。
                if crate::config::AppConfig::load_without_secrets()
                    .general
                    .daemon_lifecycle
                    == crate::config::DaemonLifecycleMode::KeepAlive
                {
                    continue;
                }
                // 空闲退出守卫（spec D18）：仅当无在途请求、无状态窗口订阅、**且**无「工作中」
                // agent 时才计空闲。空闲 agent 不保活；版本更新 drain 由 begin_drain 独立处理、不受此影响。
                // 另：有活跃 /watch 订阅时不退（卡片要持续就地刷新；订阅随 agent 结束而消亡，有界）。
                if state.active.load(Ordering::SeqCst) == 0
                    && state.agents.working_count() == 0
                    && !has_agent_subs(&state)
                    && !state
                        .watch
                        .subs
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|s| !s.rewatchable)
                {
                    let idle = state
                        .last_active
                        .lock()
                        .map(|t| t.elapsed())
                        .unwrap_or_default();
                    if idle >= timeout {
                        log("idle timeout reached; shutting down");
                        state.shutdown.notify_one();
                        break;
                    }
                }
            }
        });
    }

    // 配置实时生效（A12）：监听 config.json 变更 → 重载 + 失效改动的 Router + 通知活动 GUI。
    {
        let state = state.clone();
        let mut rx = config_watch::spawn();
        tokio::spawn(async move {
            while rx.recv().await.is_some() {
                on_config_changed(&state).await;
            }
        });
    }

    // 渠道健康表变化（R7）→ 即时推一帧 TrayState（托盘警示实时出现/消失）。
    // 回调可能来自任意渠道 task；broadcast 内部自行 spawn，无订阅者时廉价早退。
    {
        let state = state.clone();
        crate::channels::health::set_notifier(move || broadcast_tray_state(&state));
    }

    // 临时目录清理（A10）：启动即清一次，之后每小时清理过期 temp/askhuman/<id>/。
    // 顺带轮转 daemon.log（超 5MB → 挪 .1 并清空；保活 daemon 长跑也不至无限增长）。
    tokio::spawn(async move {
        loop {
            cleanup_temp_dirs();
            lifecycle::rotate_log_if_needed();
            tokio::time::sleep(Duration::from_secs(3600)).await;
        }
    });

    // 版本自更新：后台定期检查远端最新版（启动稍延迟一次，之后每 24h），有变化广播给弹窗。
    {
        let state = state.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(20)).await;
            loop {
                check_for_update(&state).await;
                tokio::time::sleep(Duration::from_secs(24 * 3600)).await;
            }
        });
    }

    // 版本自更新：周期监听盘上二进制指纹（应用内更新 / 外部 npm 更新）→ 标记待生效并广播。
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                check_pending_update(&state);
            }
        });
    }

    // Agent 生命周期：进程存活轮询 + TTL 兜底（spec D5/D12）。变化即持久化 + 推快照；
    // 有订阅窗口时每个 tick 都推（窗口的相对时间由前端自身 ticker 每秒刷新，这里只为状态变化兜底）。
    // 间隔取 15s：正常退出走事件驱动即时反映；只有 kill/崩溃这类漏事件才靠轮询兜底，
    // 15s 的判定延迟可接受，又能显著降低 daemon 空转唤醒。
    {
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(15)).await;
                // 在途 AskHuman 豁免：先给「正等待人类回答」的 agent 刷新活动，使其不被兜底降级。
                // pid 版覆盖有 pid 的 agent；session_id 版覆盖无 pid 的（Codex 共享 app-server /
                // Claude 被 scrub），两者并列（spec D25/D26 + Q1 甲）。
                state
                    .agents
                    .refresh_by_pids(&state.registry.in_flight_agent_pids());
                state
                    .agents
                    .refresh_by_session_ids(&state.registry.in_flight_agent_session_ids());
                // 进程存活轮询 + 无 pid TTL 兜底 + 「工作中」兜底超时（打断这类无 hook 场景）。
                let changed = state.agents.poll_liveness()
                    | state.agents.ttl_sweep()
                    | state.agents.working_backstop_sweep(working_backstop_secs());
                if changed {
                    state.agents.persist();
                }
                // 插话队列兜底清理（spec agent-interject D2/D8）：不再活动的会话（漏 session-end /
                // 进程死亡 / pid 轮换 / daemon 停机期间结束）清空其待送达条目、放行等待者。
                let ij_changed = state
                    .interject
                    .retain_sessions(&state.agents.active_session_ids());
                if ij_changed {
                    state.interject.persist();
                }
                if changed || ij_changed || has_agent_subs(&state) {
                    broadcast_agents_state(&state);
                }
                // 菜单栏宿主在线时周期刷新（uptime / IM 连接等随时间变化的字段）。
                if has_tray_subs(&state) {
                    broadcast_tray_state(&state);
                }
            }
        });
    }

    // 菜单栏图标已开启则兜底拉起 GUI 宿主（单实例去重；always 主要靠登录项）。
    maybe_spawn_gui_host(&state.config.lock().unwrap().clone());

    // 方案6：daemon ready 后预热一个弹窗实例（self-gated：关 / 无显示则不补）。新 daemon / 二进制换新
    // 后的首个弹窗仍冷，但其余请求都能命中热实例。
    maybe_topup_warm(&state);

    // 「存活即监听」：daemon 一就绪即在后台连启用的 IM 起入站监听（与工作中 agent / 在途提问 /
    // 自动激活开关无关），使在世期间任何 IM 消息都能被收到并回复。后台执行，不挡受理 / 弹窗关键路径；
    // 无启用 IM 时 `ensure_inbound_listeners` 自身零钥匙串直接返回。监听不计入保活，不阻止空闲退出。
    {
        let st = state.clone();
        tokio::spawn(async move {
            ensure_inbound_listeners(&st).await;
        });
    }

    // `/watch` 实时关注引擎（spec docs/specs/im-watch.md）：先恢复持久化订阅（重启后继续
    // 编辑同一张卡），再进入 Notify / 自适应 tick 循环。
    {
        let st = state.clone();
        tokio::spawn(async move {
            watch_restore_and_run(st).await;
        });
    }

    // select/confirm 路由重建旁路任务：避免 card handler → ensure_select_routes 的 async 类型环。
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                st.select.route_refresh.notified().await;
                ensure_select_routes(&st).await;
            }
        });
    }

    // `/msg` 输入卡恢复/过期引擎：启动即把上次异常退出留下的卡定格，之后每 15 秒收尾 TTL 到期卡。
    {
        let st = state.clone();
        tokio::spawn(async move {
            loop {
                finalize_expired_msg_compose_cards(&st).await;
                tokio::time::sleep(Duration::from_secs(15)).await;
            }
        });
    }

    loop {
        tokio::select! {
            _ = state.shutdown.notified() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, _addr)) => {
                    let st = state.clone();
                    tokio::spawn(async move { handle_conn(stream, st).await; });
                }
                Err(e) => log(&format!("accept error: {}", e)),
            }
        }
    }

    // Shutting down: cancel any in-flight requests so their IM cards finalize to "Cancelled".
    // Unlike CLI-disconnect, the runtime is about to exit, so we give the finalize HTTP calls a
    // brief bounded window to land (sessions may take up to ~1s to notice the cancel + ~0.3s HTTP).
    state.popup_focus.lock().unwrap().clear();
    if state.registry.cancel_all_requests() > 0 {
        tokio::time::sleep(Duration::from_millis(2000)).await;
    }

    // 第 15 轮定案：graceful 关停（drain/stop/install 换新）前把活动单选/确认卡定格「已失效」，
    // 避免重启后旧卡（台账不持久化）点击静默无响应。限时兜底，不拖关停。
    let _ = tokio::time::timeout(Duration::from_secs(8), finalize_all_select_cards(&state)).await;

    // 方案6：关停前回收热实例（drop 连接 → 热进程收 EOF 自杀，不悬挂）。先置 `draining` 再回收，
    // 否则被回收的热实例走死亡分支会 `maybe_topup_warm` 又拉起一个新热进程（孤儿，daemon 已停）。
    // `begin_drain` 走的是同一道门（先 `draining=true` 再 `recycle_warm`）；plain stop/restart 此前漏了。
    state.draining.store(true, Ordering::SeqCst);
    recycle_warm(&state);

    // 退出前持久化 agent 注册表（spec D18：换新 / 闲退后由新 daemon 重载复核）。
    state.agents.persist();

    // 收尾：主动丢弃常热 Router（其 Drop 中止 reader 任务、关闭 IM 长连接），再清理 socket/meta。
    // 进行中的请求各自持有 Router Arc 克隆，故仅在无人持有时才真正断连。
    *state.dd_router.lock().await = None;
    *state.fs_router.lock().await = None;
    *state.tg_router.lock().await = None;
    *state.sl_router.lock().await = None;
    cleanup();
    log("stopped");
    0
}

/// 管理面板操作的同步实现（blocking 线程内执行，spec codex-permission-remember §6.3）。
/// 摘要用 agent registry 增强标题 / 项目名；重置在 store 文件锁内原子完成，返回后
/// 下一次相关请求即重新弹窗（store 无内存缓存，每次匹配都重读文件）。
fn permission_rules_op(
    state: &Arc<ServerState>,
    op: ipc::PermissionRulesOp,
) -> ipc::PermissionRulesResult {
    use crate::permission_rules as store;
    match op {
        ipc::PermissionRulesOp::Summaries => {
            let sessions = store::session_summaries()
                .into_iter()
                .map(|summary| {
                    let (title, project_name) = state
                        .agents
                        .session_display(&summary.session_id)
                        .unwrap_or_default();
                    ipc::PermissionSessionGroup {
                        summary,
                        title,
                        project_name,
                    }
                })
                .collect();
            ipc::PermissionRulesResult::Summaries {
                sessions,
                global_count: store::global_rules().len(),
            }
        }
        ipc::PermissionRulesOp::SessionDetail { session_id } => ipc::PermissionRulesResult::Rules {
            rules: permission_rule_infos(store::session_rules(&session_id)),
        },
        ipc::PermissionRulesOp::GlobalDetail => ipc::PermissionRulesResult::Rules {
            rules: permission_rule_infos(store::global_rules()),
        },
        ipc::PermissionRulesOp::ResetSession { session_id } => ipc::PermissionRulesResult::Reset {
            removed: store::reset_session(&session_id),
        },
        ipc::PermissionRulesOp::ResetGlobal => ipc::PermissionRulesResult::Reset {
            removed: store::reset_global(),
        },
        ipc::PermissionRulesOp::DisableYolo { session_id } => ipc::PermissionRulesResult::Reset {
            removed: usize::from(store::disable_yolo(&session_id)),
        },
    }
}

fn permission_rule_infos(
    rules: Vec<crate::permission_rules::StoredRule>,
) -> Vec<ipc::PermissionRuleInfo> {
    use crate::permission_rules::{RuleKey, RULE_TTL_MS};
    rules
        .into_iter()
        .map(|rule| {
            let kind = match &rule.key {
                RuleKey::FileExact { .. } => "fileExact",
                RuleKey::FileProject { .. } => "fileProject",
                RuleKey::FileDisk => "fileDisk",
                RuleKey::McpTool { .. } => "mcpTool",
                RuleKey::NetworkHost { .. } => "networkHost",
                RuleKey::ShellExact { .. } => "shellExact",
                RuleKey::ShellPrefix { .. } => "shellPrefix",
                RuleKey::ShellRelaxed => "shellRelaxed",
                RuleKey::Yolo => "yolo",
            };
            let anchor = rule.last_used_at_ms.max(rule.created_at_ms);
            ipc::PermissionRuleInfo {
                kind: kind.to_string(),
                display: rule.key.display(),
                created_at_ms: rule.created_at_ms,
                last_used_at_ms: rule.last_used_at_ms,
                expires_at_ms: anchor.saturating_add(RULE_TTL_MS),
            }
        })
        .collect()
}

/// 控制阶段的产物：收到接管型消息（提交 / GUI 握手）或连接关闭。
enum Control {
    Submit(Box<TaskRequest>),
    SubmitConfirm(Box<ConfirmTask>),
    Gui(String),
    /// 方案6 预热弹窗握手：接管连接，入热池待命、等领用。
    GuiWarm,
    /// 状态窗口订阅：接管连接，持续推送 agent 快照。
    AgentsSub,
    /// 菜单栏宿主订阅：接管连接，持续推送 `TrayState`（非保活）。
    TraySub,
    /// 插话 composer 窗口连接：接管连接，登记「composer 打开」；断开＝关闭（非保活）。
    InterjectComposer {
        session_id: String,
    },
    /// 插话 Hold：hook 连接等待 composer 提交/取消（非保活；首帧 Hold 已回，等二帧）。
    InterjectHold {
        session_id: String,
        rx: tokio::sync::oneshot::Receiver<crate::agents::interject::WaitOutcome>,
    },
    Closed,
}

async fn handle_conn(stream: UnixStream, state: Arc<ServerState>) {
    state.active.fetch_add(1, Ordering::SeqCst);
    let (r, w) = stream.into_split();
    let mut reader = BufReader::new(r);
    let mut w = w;

    match control_loop(&mut reader, &mut w, &state).await {
        Control::Submit(task) => handle_submit(*task, reader, w, &state).await,
        Control::SubmitConfirm(task) => handle_submit_confirm(*task, reader, w, &state).await,
        Control::Gui(token) => handle_gui(token, reader, w, &state).await,
        Control::GuiWarm => handle_gui_warm(reader, w, &state).await,
        Control::AgentsSub => handle_agents_sub(reader, w, &state).await,
        Control::TraySub => handle_tray_sub(reader, w, &state).await,
        Control::InterjectComposer { session_id } => {
            handle_interject_composer(reader, w, &state, session_id).await
        }
        Control::InterjectHold { session_id, rx } => {
            handle_interject_hold(reader, w, &state, session_id, rx).await
        }
        Control::Closed => {}
    }

    if let Ok(mut t) = state.last_active.lock() {
        *t = Instant::now();
    }
    state.active.fetch_sub(1, Ordering::SeqCst);
}

/// 控制阶段：处理 Hello / Status / Stop（即时应答）；遇到 Submit / GuiHello 返回以便接管连接。
async fn control_loop(
    reader: &mut Reader,
    w: &mut OwnedWriteHalf,
    state: &Arc<ServerState>,
) -> Control {
    loop {
        let msg: Option<ClientMsg> = match ipc::read_msg(reader).await {
            Ok(m) => m,
            Err(e) => {
                log(&format!("read error: {}", e));
                return Control::Closed;
            }
        };
        let Some(msg) = msg else {
            return Control::Closed;
        }; // EOF / 对端关闭

        match msg {
            ClientMsg::Hello(hello) => {
                // 已在排空：一律回 Draining（客户端等下线后用新二进制拉起重试）。
                if state.draining.load(Ordering::SeqCst) {
                    let ack = HelloAck {
                        protocol_version: ipc::PROTOCOL_VERSION,
                        daemon_version: version(),
                        status: HelloStatus::Draining,
                        reason: Some("draining: waiting for active requests".to_string()),
                    };
                    let _ = ipc::write_msg(w, &ServerMsg::HelloAck(ack)).await;
                    continue;
                }
                let now_fp = lifecycle::current_fingerprint();
                // 指纹按「内容哈希」比对（与路径/mtime 无关）：
                // 自身盘上二进制内容被换 / 客户端二进制内容不一致 / 协议不一致 → 过时，让位换新。
                let stale = now_fp != state.startup_fp
                    || hello.fingerprint != state.startup_fp
                    || hello.protocol_version != ipc::PROTOCOL_VERSION;
                let auto_restart = std::env::var("ASKHUMAN_DAEMON_AUTORESTART")
                    .map(|v| v != "0")
                    .unwrap_or(true);
                // 过时且有在途请求 → 进入排空（不打断在途）；无在途 → 立即换新（零延迟）。
                let draining = stale && auto_restart && state.registry.active_count() > 0;
                let restarting = stale && auto_restart && !draining;
                let ack = HelloAck {
                    protocol_version: ipc::PROTOCOL_VERSION,
                    daemon_version: version(),
                    status: if draining {
                        HelloStatus::Draining
                    } else if restarting {
                        HelloStatus::Restarting
                    } else {
                        HelloStatus::Ok
                    },
                    reason: if stale && auto_restart {
                        Some("binary or protocol changed".to_string())
                    } else {
                        None
                    },
                };
                let _ = ipc::write_msg(w, &ServerMsg::HelloAck(ack)).await;
                if draining {
                    log("stale binary/protocol detected with active requests; draining");
                    begin_drain(state);
                    continue;
                }
                if restarting {
                    log("stale binary/protocol detected; shutting down for restart");
                    state.shutdown.notify_one();
                    return Control::Closed;
                }
            }
            ClientMsg::Status => {
                let info = StatusInfo {
                    pid: std::process::id(),
                    version: version(),
                    protocol_version: ipc::PROTOCOL_VERSION,
                    uptime_secs: now_secs().saturating_sub(state.started_at),
                    socket: transport::socket_path().display().to_string(),
                    active_requests: state.registry.active_count(),
                    im_connections: active_im_connections(state).await,
                    draining: state.draining.load(Ordering::SeqCst),
                    channel_issues: channel_issue_infos(),
                };
                let _ = ipc::write_msg(w, &ServerMsg::Status(info)).await;
            }
            ClientMsg::Stop { force } => {
                let _ = ipc::write_msg(w, &ServerMsg::Stopping).await;
                // 默认 graceful：有在途请求时排空后退出；`--force` 或无在途 → 立即退出。
                if !force && state.registry.active_count() > 0 {
                    log("graceful stop requested; draining");
                    begin_drain(state);
                    return Control::Closed;
                }
                log("stop requested");
                state.shutdown.notify_one();
                return Control::Closed;
            }
            ClientMsg::Submit(task) => return Control::Submit(Box::new(task)),
            ClientMsg::SubmitConfirm(task) => return Control::SubmitConfirm(task),
            ClientMsg::GuiHello { token } => return Control::Gui(token),
            // 方案6 预热弹窗握手（无 token）：接管连接入热池待命。
            ClientMsg::GuiWarmReady => return Control::GuiWarm,
            // Agent 生命周期事件上报（默认即发即走；`interject_poll=true` 时回一帧插话裁决）：
            // 更新注册表，变化则持久化 + 推订阅窗口。
            ClientMsg::AgentEvent {
                agent,
                event,
                session_id,
                pid,
                hint_pid,
                cwd,
                launch_id,
                prompt_sha256,
                ts,
                tool,
                interject_poll,
            } => {
                if let (Some(kind), Some(ev)) =
                    (AgentKind::parse(&agent), LifecycleEvent::parse(&event))
                {
                    // PreToolUse：先回 interject 再做后续（hook 在等，优先解除阻塞）。
                    if interject_poll {
                        use crate::agents::interject::PollOutcome;
                        use crate::ipc::InterjectAction;
                        match state.interject.poll(&session_id) {
                            PollOutcome::None => {
                                let _ = ipc::write_msg(
                                    w,
                                    &ServerMsg::InterjectDecision {
                                        action: InterjectAction::None,
                                        text: String::new(),
                                    },
                                )
                                .await;
                            }
                            PollOutcome::Message {
                                text,
                                receipt_channels,
                            } => {
                                let _ = ipc::write_msg(
                                    w,
                                    &ServerMsg::InterjectDecision {
                                        action: InterjectAction::Message,
                                        text,
                                    },
                                )
                                .await;
                                state.interject.persist();
                                broadcast_agents_state(state);
                                spawn_read_receipts(state, &session_id, receipt_channels);
                            }
                            PollOutcome::Hold(rx) => {
                                let _ = ipc::write_msg(
                                    w,
                                    &ServerMsg::InterjectDecision {
                                        action: InterjectAction::Hold,
                                        text: String::new(),
                                    },
                                )
                                .await;
                                return Control::InterjectHold { session_id, rx };
                            }
                        }
                    }

                    // PID 解析：优先用显式 pid，否则从 hint_pid 走缓存/walk。
                    let resolved_pid =
                        pid.or_else(|| state.agents.resolve_pid(&session_id, kind, hint_pid));

                    let event_cwd = cwd.clone();
                    let changed =
                        state
                            .agents
                            .apply_event(kind, ev, &session_id, resolved_pid, cwd, ts);
                    if changed {
                        state.agents.persist();
                        broadcast_agents_state(state);
                    }
                    if matches!(ev, LifecycleEvent::SessionStart | LifecycleEvent::TurnStart) {
                        if let Some(ref path) = event_cwd {
                            let _ =
                                crate::agents::workspaces::add(std::path::Path::new(path), false);
                        }
                    }
                    if matches!(ev, LifecycleEvent::SessionEnd) {
                        state.agents.clear_pid_cache(&session_id);
                        if state.interject.remove_session(&session_id) {
                            state.interject.persist();
                        }
                    }
                    match tool {
                        Some(crate::ipc::ToolReport {
                            phase: crate::ipc::ToolPhase::Pre,
                            name,
                            object,
                        }) => state.agents.set_current_tool(
                            kind,
                            &session_id,
                            resolved_pid,
                            name,
                            object,
                        ),
                        Some(crate::ipc::ToolReport {
                            phase: crate::ipc::ToolPhase::Post,
                            ..
                        }) => state.agents.clear_current_tool(kind, &session_id),
                        None => {}
                    }
                    ensure_inbound_listeners(state).await;
                    state.watch.notify.notify_one();
                    if matches!(ev, LifecycleEvent::TurnStart) {
                        match_pending_launch_watch(
                            state,
                            kind,
                            &session_id,
                            event_cwd.as_deref(),
                            launch_id.as_deref(),
                            prompt_sha256.as_deref(),
                        )
                        .await;
                    }
                } else if interject_poll {
                    let _ = ipc::write_msg(
                        w,
                        &ServerMsg::InterjectDecision {
                            action: crate::ipc::InterjectAction::None,
                            text: String::new(),
                        },
                    )
                    .await;
                }
            }
            ClientMsg::McpInstanceRegister {
                mcp_instance_id,
                project,
                server_pid,
                parent_pid_hint,
            } => {
                state
                    .grok_bindings
                    .register(mcp_instance_id, project, server_pid, parent_pid_hint);
            }
            ClientMsg::GrokBindingPending {
                agent_session_id,
                qualified_tool_name,
                arguments_sha256,
                project,
                hook_parent_hint,
                tool_use_id,
                created_at_ms,
            } => {
                state.grok_bindings.add_pending(
                    agent_session_id,
                    qualified_tool_name,
                    arguments_sha256,
                    project,
                    hook_parent_hint,
                    tool_use_id,
                    created_at_ms,
                );
            }
            ClientMsg::GrokBindingClaim {
                mcp_instance_id,
                project,
                tool_name,
                arguments_sha256,
                server_pid,
            } => {
                let agent_session_id = state.grok_bindings.claim(
                    &mcp_instance_id,
                    &project,
                    &tool_name,
                    &arguments_sha256,
                    server_pid,
                );
                let _ = ipc::write_msg(w, &ServerMsg::GrokBindingClaim { agent_session_id }).await;
            }
            // 状态窗口订阅：接管连接持续推送。
            ClientMsg::AgentsSubscribe => return Control::AgentsSub,
            // 焦点会话消息只在 agents 订阅连接上有意义（handle_agents_sub 处理）；此处忽略。
            ClientMsg::AgentsFocus { .. } => {}
            // 菜单栏宿主订阅：接管连接持续推送 TrayState（非保活）。
            ClientMsg::TraySubscribe => return Control::TraySub,
            ClientMsg::RefreshUpdateState => refresh_update_snapshot(state),
            // GUI 启动新任务后切活跃槽到 popup（spec gui-agent-task-launch G11）：走统一
            // set_active_channel（自然获得旧 IM 反激活回执与 auto-end-watch 语义），无回包。
            ClientMsg::ActivatePopupSlot => {
                let _ = set_active_channel(state, "popup").await;
            }
            // 托盘「待答」子菜单点击：聚焦/闪烁对应请求的弹窗（即发即走，无回包）。
            ClientMsg::FocusRequest { request_id } => {
                update_popup_focus(state, |focus| focus.claim_and_focus(&request_id));
            }
            // 状态窗口手动把某 agent 置空闲（纠正漏 hook 卡「工作中」）：变化则持久化 + 推订阅窗口。
            ClientMsg::AgentForceIdle { session_id } => {
                if state.agents.force_idle(&session_id) {
                    state.agents.persist();
                    broadcast_agents_state(state);
                }
            }
            // 插话 composer 窗口登记：接管连接（连接断开＝关闭，非保活）。
            ClientMsg::InterjectComposer { session_id } => {
                return Control::InterjectComposer { session_id };
            }
            // 插话提交（独立连接形态；composer 连接上的提交在 handle_interject_composer 处理）。
            ClientMsg::InterjectSubmit { session_id, text } => {
                interject_submit(state, &session_id, &text);
            }
            // 插话追加（不覆盖既有待送达；快捷固定插话用）。
            ClientMsg::InterjectAppend { session_id, text } => {
                interject_append(state, &session_id, &text);
            }
            // 撤回待送达。
            ClientMsg::InterjectClear { session_id } => {
                if state.interject.clear(&session_id) {
                    state.interject.persist();
                    broadcast_agents_state(state);
                }
            }
            // 查询待送达全文（composer 预填 / IM 回显）。
            ClientMsg::InterjectQuery { session_id } => {
                let _ = ipc::write_msg(
                    w,
                    &ServerMsg::InterjectState {
                        text: state.interject.full_text(&session_id),
                        entries: state.interject.pending_count(&session_id),
                    },
                )
                .await;
            }
            // 权限授权管理面板（spec codex-permission-remember §6.3）：daemon 是 rule store 的
            // 唯一写入方；store 操作走文件锁（阻塞），放 blocking 线程执行，完成后回一帧。
            ClientMsg::PermissionRules { op } => {
                let result = {
                    let state = state.clone();
                    tokio::task::spawn_blocking(move || permission_rules_op(&state, op)).await
                };
                match result {
                    Ok(result) => {
                        let _ = ipc::write_msg(w, &ServerMsg::PermissionRules { result }).await;
                    }
                    Err(_) => {
                        let _ = ipc::write_msg(
                            w,
                            &ServerMsg::Error {
                                message: "permission rules task failed".to_string(),
                            },
                        )
                        .await;
                    }
                }
            }
            // 自动识别（Q6）：就地处理（可能阻塞至多 120s 等用户发码），完成后回结果继续循环。
            // 排空期拒绝（兜底；正常情况下客户端在 Hello 即被挡住而回退进程内识别）。
            ClientMsg::Detect(req) => {
                if state.draining.load(Ordering::SeqCst) {
                    let _ = ipc::write_msg(
                        w,
                        &ServerMsg::Error {
                            message: "daemon is draining".to_string(),
                        },
                    )
                    .await;
                } else {
                    handle_detect(&req, state, reader, w).await
                }
            }
            // Answer 只应在 GUI 接管阶段出现；控制阶段收到即忽略。
            ClientMsg::Answer { .. }
            | ClientMsg::ConfirmAnswer { .. }
            | ClientMsg::ConfirmReady { .. }
            | ClientMsg::PopupReady { .. }
            | ClientMsg::PopupFocused { .. }
            | ClientMsg::PopupDismissed { .. } => {}
        }
    }
}

/// 进入排空状态（幂等）：首次置位时 spawn 看门狗，在途请求全部完结后触发退出。
fn begin_drain(state: &Arc<ServerState>) {
    if state.draining.swap(true, Ordering::SeqCst) {
        return; // 已在排空。
    }
    // 方案6：进入排空即回收热实例（它是旧二进制；新 daemon 起来后再补热）。`draining` 已置位，
    // 故后续不会再补热；排空期到来的请求走冷路径（现有 drain 语义）。
    recycle_warm(state);
    broadcast_tray_state(state);
    let state = state.clone();
    tokio::spawn(async move {
        loop {
            if state.registry.active_count() == 0 {
                log("drain complete; shutting down");
                state.shutdown.notify_one();
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    });
}

/// Accept a structured confirmation task and return only a validated terminal decision.
/// Surface attachment is layered on top of this lifecycle; until one becomes ready, callers
/// receive an explicit fallback and continue with the agent's native approval UI.
async fn handle_submit_confirm(
    task: ConfirmTask,
    mut reader: Reader,
    mut w: OwnedWriteHalf,
    state: &Arc<ServerState>,
) {
    if state.draining.load(Ordering::SeqCst) {
        let _ = ipc::write_msg(
            &mut w,
            &ServerMsg::ConfirmFallback {
                reason: ConfirmFallbackReason::Draining,
            },
        )
        .await;
        return;
    }

    // Permission memory auto-allow (spec codex-permission-remember §6): answer from stored
    // shadow rules before engaging any surface. A hit refreshes the rules' rolling retention.
    // Relaxed mode (D52) piggybacks here: an eligible shell request (hook-side verdict on
    // the query) auto-allows when the global toggle or a session ShellRelaxed rule is on.
    // YOLO mode (D53) is the last check and needs no query: any Codex permission task
    // (memory metadata present) auto-allows while the session Yolo rule is on. Relaxed and
    // yolo allows are audit-logged with the request substance.
    if task.agent_kind == "codex" && task.memory.is_some() {
        let query = task.memory.as_ref().and_then(|memory| memory.query.clone());
        // Audit text for a YOLO allow: the concrete query target when present, else the
        // popup summary (e.g. dangerous command popups carry no query).
        let yolo_detail = match &query {
            Some(crate::permission_rules::MemoryQuery::ShellCommands { commands, .. }) => commands
                .iter()
                .map(|argv| argv.join(" "))
                .collect::<Vec<_>>()
                .join(" && "),
            Some(crate::permission_rules::MemoryQuery::FileEdit { paths }) => paths.join(", "),
            Some(crate::permission_rules::MemoryQuery::McpTool { tool }) => tool.clone(),
            Some(crate::permission_rules::MemoryQuery::NetworkHost {
                host,
                protocol,
                port,
            }) => format!("{protocol}://{host}:{port}"),
            None => format!(
                "{} — {}",
                task.spec
                    .context
                    .iter()
                    .find(|field| field.id == "tool")
                    .map(|field| field.value.as_str())
                    .unwrap_or("?"),
                task.spec.detail.summary
            ),
        };
        let session_id = task.agent_session_id.clone();
        let relaxed_global = state.config_snapshot().permissions.codex_relaxed_shell;
        let hit = tokio::task::spawn_blocking(move || {
            if let Some(query) = &query {
                if crate::permission_rules::check_auto_allow(&session_id, query) {
                    return Some(("memory", String::new()));
                }
                if let crate::permission_rules::MemoryQuery::ShellCommands {
                    commands,
                    relaxed_eligible: true,
                } = query
                {
                    if relaxed_global
                        || crate::permission_rules::check_relaxed_auto_allow(&session_id)
                    {
                        let scope = if relaxed_global { "global" } else { "session" };
                        let script = commands
                            .iter()
                            .map(|argv| argv.join(" "))
                            .collect::<Vec<_>>()
                            .join(" && ");
                        return Some((scope, script));
                    }
                }
            }
            if crate::permission_rules::check_yolo_auto_allow(&session_id) {
                return Some(("yolo", yolo_detail));
            }
            None
        })
        .await
        .unwrap_or(None);
        if let Some((source, detail)) = hit {
            match source {
                "memory" => log(&format!(
                    "permission memory auto-allow for session {}",
                    task.agent_session_id
                )),
                "yolo" => log(&format!(
                    "yolo auto-allow for session {}: {detail}",
                    task.agent_session_id
                )),
                _ => log(&format!(
                    "relaxed auto-allow ({source}) for session {}: {detail}",
                    task.agent_session_id
                )),
            }
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::ConfirmFinal {
                    result: crate::models::ConfirmResult {
                        action_id: crate::permission_rules::AUTO_ALLOW_ACTION_ID.to_string(),
                        comment: None,
                        source_channel_id: "memory".to_string(),
                    },
                },
            )
            .await;
            return;
        }
    }

    let (entry, mut final_rx) = match state.registry.create_confirm(task) {
        Ok(created) => created,
        Err(error) => {
            log(&format!("invalid confirmation request: {error}"));
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::ConfirmFallback {
                    reason: ConfirmFallbackReason::InvalidRequest,
                },
            )
            .await;
            return;
        }
    };
    let request_id = entry.request_id.clone();
    if ipc::write_msg(
        &mut w,
        &ServerMsg::ConfirmAccepted {
            request_id: request_id.clone(),
        },
    )
    .await
    .is_err()
    {
        entry.coordinator.cancel();
        entry.cancel.notify_waiters();
        state.registry.remove_confirm(&request_id);
        return;
    }
    broadcast_tray_state(state);

    // 缓存快照（密钥已解析）：零钥匙串、零磁盘读，见 `config_snapshot`。
    let config = state.config_snapshot();
    let popup_enabled = popup_should_dispatch(&config, has_display());
    let im_candidates = confirm_im_candidates(&entry, state, &config, popup_enabled);
    if popup_enabled {
        entry.start_delivery("popup");
    }
    for channel in &im_candidates {
        entry.start_delivery(*channel);
    }
    if !popup_enabled && im_candidates.is_empty() {
        entry
            .coordinator
            .fallback(ConfirmFallbackReason::NoAvailableChannel);
    }

    if popup_enabled {
        if dispatch_interaction_popup(InteractionEntry::Confirm(entry.clone()), state, "", false) {
            spawn_confirm_popup_watchdog(entry.clone());
        } else if entry.mark_failed("popup", "failed to spawn popup helper") {
            entry
                .coordinator
                .fallback(ConfirmFallbackReason::NoAvailableChannel);
        }
    }
    attach_confirm_im_channels(&entry, state, &config, &im_candidates).await;
    for channel in &im_candidates {
        mark_watch_disturbed(state, channel);
    }
    ensure_inbound_listeners(state).await;

    let outcome = tokio::select! {
        outcome = final_rx.recv() => outcome,
        _ = tokio::time::sleep_until(entry.deadline) => {
            entry.coordinator.fallback(ConfirmFallbackReason::Expired);
            final_rx.recv().await
        }
        _ = wait_cli_eof(&mut reader) => {
            entry.coordinator.cancel();
            update_popup_focus(state, |focus| focus.terminal(&request_id));
            entry.cancel.notify_waiters();
            state.registry.remove_confirm(&request_id);
            broadcast_tray_state(state);
            return;
        }
    };

    match outcome {
        Some(ConfirmOutcome::Final(result)) => {
            let _ = ipc::write_msg(&mut w, &ServerMsg::ConfirmFinal { result }).await;
        }
        Some(ConfirmOutcome::Fallback(reason)) => {
            let _ = ipc::write_msg(&mut w, &ServerMsg::ConfirmFallback { reason }).await;
        }
        None => {
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::ConfirmFallback {
                    reason: ConfirmFallbackReason::InternalError,
                },
            )
            .await;
        }
    }
    if config.channels.auto_activation {
        if let Some(winner) = entry.coordinator.winner_channel_id() {
            set_active_channel(state, &winner).await;
        }
    }
    entry.mark_deliveries_terminal();
    update_popup_focus(state, |focus| focus.terminal(&request_id));
    entry.cancel.notify_waiters();
    state.registry.remove_confirm(&request_id);
    broadcast_tray_state(state);
    for channel in ["feishu", "telegram", "slack", "dingding"] {
        if entry.has_delivery(channel) {
            for subscription in state
                .watch
                .subs
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|subscription| subscription.channel == channel)
            {
                subscription.last_move_ms = 0;
            }
        }
    }
    state.watch.notify.notify_one();
    log(&format!("confirmation request {request_id} done"));
}

/// CLI 提交一次任务：建请求、spawn GUI Helper、流式回结果；CLI 断开则取消。
async fn handle_submit(
    task: TaskRequest,
    mut reader: Reader,
    mut w: OwnedWriteHalf,
    state: &Arc<ServerState>,
) {
    // 性能埋点关联 id（CLI 透传；空=关闭）。daemon 自身启动时无 ASKHUMAN_PERF env，
    // 故各阶段标记一律以本字段非空为开关（见 perf 模块说明）。
    let perf_id = task.perf_id.clone();
    let perf_autodismiss = task.perf_autodismiss;
    crate::perf::mark(&perf_id, "dmn.submit_recv");
    // 排空闸门（兜底，覆盖「Hello Ok → Submit 间隙开始排空」的竞态）：拒绝并断开，
    // 客户端等本 Daemon 下线后用新二进制重新提交。
    if state.draining.load(Ordering::SeqCst) {
        let _ = ipc::write_msg(
            &mut w,
            &ServerMsg::Draining {
                active: state.registry.active_count(),
            },
        )
        .await;
        return;
    }
    // 「自动激活」开启时：每次提问按「工作中」兜底登记会话（无 turn hook 也能驱动入站监听 / 切槽）；
    // 开关关时保持旧行为（仅刷新已追踪会话的活动，尊重「未装 hook = 不追踪」，不污染注册表）。
    let auto = AppConfig::load_without_secrets().channels.auto_activation;
    // 方案5(b)：家族 + 会话来自 CLI 的 env 探测（即时、零 ps）；进程 pid 改由 daemon 从 `caller_pid`
    // 异步 walk（见下 spawn_agent_resolve）。这里先用「能即时拿到的」刷新一次（env 有家族+会话 →
    // 按 session 刷新，pid 此刻通常为 None；旧 CLI 仍可能带 pid），async 完成后再补 pid / MCP 兜底。
    let kind_env = task.agent_kind.as_deref().and_then(AgentKind::parse);
    let sid_env = task.agent_session_id.clone();
    let from_mcp = task.from_mcp;
    let caller_pid = task.caller_pid;
    let cwd = Some(task.project.clone()).filter(|s| !s.trim().is_empty());
    let changed = match (kind_env, sid_env.clone(), task.agent_pid) {
        // 有 session_id（shell 工具子进程能从 env 拿到）：按 session 刷新。
        (Some(kind), Some(sid), pid) => {
            // MCP 工具调用不单独创建 lifecycle session；可信的每调用绑定仅用于
            // 刷新已由 lifecycle hook 追踪的 session。
            if auto && !from_mcp {
                state.agents.upsert_working(kind, &sid, pid, cwd.clone())
            } else {
                state.agents.touch_activity(kind, &sid, pid)
            }
        }
        // 无 session_id 但有 pid（旧 CLI 的 MCP 路径仍可能带 pid）：按 pid 刷新（只更新、绝不新建）。
        (Some(kind), None, Some(pid)) => state.agents.touch_activity_by_pid(kind, pid),
        _ => false,
    };
    if changed {
        state.agents.persist();
        broadcast_agents_state(state);
    }
    let lang = Lang::resolve(&task.lang);

    // 重复提问收敛（spec duplicate-ask-coalescing）：agent 那一轮被中断后往往原样重发同一个
    // 提问，而旧的 CLI 进程不会被杀，旧卡片仍然有效。这里在建新请求之前先看两件事——
    // 同一个提问是不是还挂在人面前（合流），或者刚刚已经被回答过（重放）。
    if let Some(session_key) = ask_dedup::session_key(&task) {
        let fingerprint = ask_dedup::fingerprint(&task);
        if let Some((entry, follower_rx)) = state.registry.try_attach(&session_key, &fingerprint) {
            handle_coalesced(entry, follower_rx, reader, w).await;
            return;
        }
        let hit =
            state
                .replay
                .lock()
                .unwrap()
                .get_fresh(&session_key, &fingerprint, Instant::now());
        if let Some(hit) = hit {
            let json = task.output_format == crate::models::OutputFormat::Json;
            let outcome = ask_dedup::mark_replayed(&hit.outcome, hit.ago, json, lang);
            log(&format!(
                "request {} replayed ({}s ago)",
                hit.request_id,
                hit.ago.as_secs()
            ));
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::Accepted {
                    request_id: hit.request_id,
                },
            )
            .await;
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::Final {
                    stdout: outcome.stdout,
                    exit_code: outcome.exit_code,
                },
            )
            .await;
            return;
        }
    }

    let (entry, mut final_rx) = state.registry.create(task);
    let request_id = entry.request_id.clone();
    let session_key = entry.session_key.clone();
    crate::perf::mark(&perf_id, "dmn.created");
    log(&format!("request {} accepted", request_id));

    // Fire `ask-received` once per accepted request, independent of popup state.
    crate::hooks::fire_ask_received(
        &request_id,
        &entry.show.source,
        &entry.show.project,
        entry.request(),
    );

    if ipc::write_msg(
        &mut w,
        &ServerMsg::Accepted {
            request_id: request_id.clone(),
        },
    )
    .await
    .is_err()
    {
        state.registry.remove(&request_id);
        return;
    }
    crate::perf::mark(&perf_id, "dmn.accepted");
    // 在途请求数 +1：刷新菜单栏状态（待答数 / 图标圆点）。
    broadcast_tray_state(state);
    // /watch：提问创建 → 被关注 agent 可能进入「正在等待你的回答」，即时进卡。
    state.watch.notify.notify_one();

    // 按接收请求时的 Daemon 配置快照决定是否投放 Popup。配置禁用或无显示
    // 环境时必须完全跳过 Helper，让 IM 成为唯一作答渠道。
    let popup_enabled = popup_should_dispatch(&state.config_snapshot(), has_display());
    // 方案3（spec §6.1）：尽早 spawn GUI Helper（独立短命进程，带一次性 token），让其 WebView
    // 初始化与下面的「入站监听 + IM 建连」并行——token 在 `registry.create()` 即登记，helper 可
    // 立即连上，不存在「helper 先连、entry 未注册」竞态。冷启动下这把 IM 建连（数百 ms）整段移出
    // 弹窗端到端关键路径。
    // 方案6：优先领用热池中的预热弹窗（秒级上屏）；池空 / 关 / 无显示 / holder 死 → 回退冷 spawn。
    let popup_ok = popup_enabled && dispatch_popup(&entry, state, &perf_id, perf_autodismiss);
    if popup_enabled && !popup_ok {
        let _ = ipc::write_msg(
            &mut w,
            &ServerMsg::Warn {
                text: format!("{}failed to spawn popup", crate::i18n::err_prefix(lang)),
            },
        )
        .await;
    }

    // 方案5(b)：从 caller_pid 异步向上 walk 进程树解析 agent（家族 + pid，含 env 判不出时的 MCP 兜底），
    // 完成后补刷注册表活动并把结果后推弹窗 badge。整段在独立任务里跑，绝不阻塞本请求的关键路径。
    let agent_resolution = spawn_agent_resolve(
        entry.clone(),
        state.clone(),
        caller_pid,
        kind_env,
        sid_env,
        from_mcp,
        auto,
        cwd,
    );

    // 以下都已不在弹窗关键路径上（与上面已 spawn 的 helper 并行执行）：
    // 确保入站消费在线（自身按「有工作中 agent」自门控；与开关无关，使 /status 等命令独立可用）。
    ensure_inbound_listeners(state).await;
    // 挂接可用的 IM 渠道（钉钉/…）到本请求的协调器，与弹窗并行抢答。
    let im_attached = attach_im_channels(
        &entry,
        state,
        &mut w,
        lang,
        popup_enabled,
        kind_env,
        from_mcp,
        agent_resolution,
    )
    .await;
    crate::perf::mark(&perf_id, "dmn.im_done");
    // /watch 跟底：提问卡即将出现在渠道会话里，是一次「非 watch」扰动（提问期间跟底被抑制，
    // 这里先记水位线，供答复完结后立即跟底）。
    for ch in ["feishu", "telegram", "slack"] {
        if entry.coordinator.has_channel(ch) {
            mark_watch_disturbed(state, ch);
        }
    }
    // IM 长连接可能在此刚建立，刷新菜单栏「已连 IM」。
    broadcast_tray_state(state);

    // 既无弹窗也无 IM 渠道 → 无可用渠道，按错误收尾。
    if !popup_ok && !im_attached {
        let _ = ipc::write_msg(
            &mut w,
            &ServerMsg::Final {
                stdout: String::new(),
                exit_code: request::EXIT_NO_CHANNEL,
            },
        )
        .await;
        state.registry.remove(&request_id);
        return;
    }

    // 看门狗：弹窗已拉起但限定时间内未连上 → 判失败；但若已挂了 IM 渠道则不致命（让 IM 继续等答）。
    if popup_ok {
        spawn_gui_watchdog(entry.clone(), lang, im_attached, state.clone());
    }

    // 等待结果或 CLI 断开。本连接断开时只有在**没有其它调用方还在等**这份结果时才取消整个
    // 请求（spec duplicate-ask-coalescing D4）：合流上来的调用方仍需要人回答完的答案。
    let mut caller_gone = false;
    let outcome = loop {
        // 本连接已断且再无人等这份结果 → 取消整个请求（最后离场的可能是合流上来的调用方，
        // 它只会唤醒这里，收尾统一在提交连接做）。
        if caller_gone && entry.waiter_count() == 0 {
            log(&format!(
                "request {} cli disconnected; cancelling",
                request_id
            ));
            // Cancel the whole request: IM cards finalize to "Cancelled by caller", popup closes.
            // The IM finalize runs in the channels' own tasks (which outlive this entry), so the
            // daemon need not wait here — it stays alive.
            let caller = crate::i18n::tr(lang, "channel.sourceCaller").to_string();
            entry.coordinator.cancel_request(caller, "caller");
            update_popup_focus(state, |focus| focus.terminal(&request_id));
            entry.cancel.notify_waiters();
            state.registry.remove(&request_id);
            // 不标记扰动：取消只是就地 PATCH 提问卡定格，不产生新消息（不淹没 watch 卡）。
            state.watch.notify.notify_one();
            return;
        }
        let waiters_gone = entry.waiters_gone.clone();
        tokio::select! {
            o = final_rx.recv() => break o,
            _ = waiters_gone.notified(), if caller_gone => continue,
            _ = wait_cli_eof(&mut reader), if !caller_gone => {
                caller_gone = true;
                if entry.release_waiter() > 0 {
                    log(&format!(
                        "request {} cli disconnected; kept for {} coalesced caller(s)",
                        request_id,
                        entry.waiter_count()
                    ));
                }
                continue;
            }
        }
    };

    // 先把结果分发给合流上来的调用方，再走本连接自己的收尾。
    if let Some(o) = outcome.as_ref() {
        entry.broadcast_outcome(o);
        // 真实作答才进重放缓存：取消的语义是「重新问我」，重放会把 agent 卡进死循环
        // （spec duplicate-ask-coalescing D6）。
        if let Some(key) = session_key.as_ref() {
            if entry.coordinator.answered() {
                state.replay.lock().unwrap().put(
                    key,
                    ask_dedup::ReplayEntry {
                        fingerprint: entry.fingerprint.clone(),
                        outcome: o.clone(),
                        request_id: request_id.clone(),
                        finished_at: Instant::now(),
                    },
                );
            }
        }
    }
    if caller_gone {
        // 原始调用方早已离场（它的连接是死的）；结果已交给还在等的那些调用方，这里只做收尾。
        finish_request_bookkeeping(state, &entry, &request_id, auto).await;
        return;
    }

    match outcome {
        Some(o) => {
            if let Some(err) = &o.stderr {
                let _ = ipc::write_msg(&mut w, &ServerMsg::Warn { text: err.clone() }).await;
            }
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::Final {
                    stdout: o.stdout,
                    exit_code: o.exit_code,
                },
            )
            .await;
        }
        None => {
            // 渲染通道意外关闭：判异常退出码 3。
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::Final {
                    stdout: String::new(),
                    exit_code: 3,
                },
            )
            .await;
        }
    }
    finish_request_bookkeeping(state, &entry, &request_id, auto).await;
}

/// 一次提问完结后的公共收尾：活跃槽、弹窗焦点、登记表、菜单栏与 `/watch` 跟底。
/// 原始调用方是否还连着都要走这一段（合流场景下它可能早就断开了）。
async fn finish_request_bookkeeping(
    state: &Arc<ServerState>,
    entry: &Arc<request::RequestEntry>,
    request_id: &str,
    auto: bool,
) {
    // 「在哪个渠道作答就用哪个」：把活跃槽更新为本次作答渠道（弹窗作答 → "popup"，即不再发 IM）。
    // 若由此从某 IM 切走，旧 IM 在 set_active_channel 内收反激活提示。仅自动激活开时生效。
    if auto {
        if let Some(winner) = entry.coordinator.winner_channel_id() {
            set_active_channel(state, &winner).await;
        }
    }
    update_popup_focus(state, |focus| focus.terminal(request_id));
    entry.cancel.notify_waiters();
    state.registry.remove(request_id);
    // 在途请求数 -1：刷新菜单栏状态。
    broadcast_tray_state(state);
    // /watch：答复完结 → 「正在等待你的回答」状态解除，即时进卡。对参与了本次问答的渠道，
    // 清零该渠道订阅的跟底节流：**已被提问卡淹没**的卡在下一次内容变化时立即跟底重发
    // （用户定案：答完能马上在底部看到自己的回答产生的更新）。**不**标记扰动——作答本身
    // 只是就地编辑提问卡、不产生新消息；提问期间新发的 watch 卡仍在底部，重发只会造出
    // 连续两张卡（验收反馈修正）。淹没判定完全依据提问卡发出时刻（attach 处已标记）。
    for ch in ["feishu", "telegram", "slack"] {
        if entry.coordinator.has_channel(ch) {
            for s in state
                .watch
                .subs
                .lock()
                .unwrap()
                .iter_mut()
                .filter(|s| s.channel == ch)
            {
                s.last_move_ms = 0;
            }
        }
    }
    state.watch.notify.notify_one();
    log(&format!("request {} done", request_id));
}

/// 合流上来的调用方（spec duplicate-ask-coalescing D3）：同会话的同一个提问已经挂在人面前，
/// 这里只等那张卡的结果，不再弹窗、不投 IM、不碰托盘 / watch / 用户 hook——它们都属于
/// 「那一次提问」，已经发生过了。本连接断开也只是少一个等待者，绝不取消别人还在等的卡片。
async fn handle_coalesced(
    entry: Arc<request::RequestEntry>,
    mut follower_rx: tokio::sync::mpsc::UnboundedReceiver<crate::app::RenderOutcome>,
    mut reader: Reader,
    mut w: OwnedWriteHalf,
) {
    let request_id = entry.request_id.clone();
    log(&format!(
        "request {} coalesced (waiters={})",
        request_id,
        entry.waiter_count()
    ));
    if ipc::write_msg(
        &mut w,
        &ServerMsg::Accepted {
            request_id: request_id.clone(),
        },
    )
    .await
    .is_err()
    {
        entry.release_waiter();
        return;
    }

    let outcome = tokio::select! {
        o = follower_rx.recv() => o,
        _ = wait_cli_eof(&mut reader) => {
            entry.release_waiter();
            log(&format!("request {} coalesced caller disconnected", request_id));
            return;
        }
    };

    match outcome {
        Some(o) => {
            if let Some(err) = &o.stderr {
                let _ = ipc::write_msg(&mut w, &ServerMsg::Warn { text: err.clone() }).await;
            }
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::Final {
                    stdout: o.stdout,
                    exit_code: o.exit_code,
                },
            )
            .await;
        }
        // 结果通道关闭（请求被取消）：与主路径同口径判异常退出码。
        None => {
            let _ = ipc::write_msg(
                &mut w,
                &ServerMsg::Final {
                    stdout: String::new(),
                    exit_code: 3,
                },
            )
            .await;
        }
    }
    entry.release_waiter();
}

/// GUI Helper 连接：凭 token 关联请求，随后进入 `serve_gui`（下发 show、收 answer 投递协调器）。
async fn handle_gui(token: String, reader: Reader, w: OwnedWriteHalf, state: &Arc<ServerState>) {
    let Some(entry) = state.registry.attach_gui(&token) else {
        log("gui hello with unknown token; closing");
        return;
    };
    // GUI 发送端：专用写任务串行写出 ServerMsg，供 show 下发与 adapter cancel 共用。
    let (gui_tx, mut gui_rx) = tokio::sync::mpsc::unbounded_channel::<ServerMsg>();
    let mut w = w;
    let writer = tokio::spawn(async move {
        while let Some(msg) = gui_rx.recv().await {
            if ipc::write_msg(&mut w, &msg).await.is_err() {
                break;
            }
        }
    });
    match entry {
        InteractionEntry::Ask(entry) => serve_gui(entry, reader, gui_tx, writer, state).await,
        InteractionEntry::Confirm(entry) => {
            serve_confirm_gui(entry, reader, gui_tx, writer, state).await
        }
    }
}

/// 冷/热弹窗共用的「服务」尾段：登记 `entry.gui` 写端、下发 show、补发 AgentResolved/UpdateState，
/// 随后读 GUI 答复 / 等取消通知，收尾清空 `entry.gui` 槽并结束写任务（→ Helper EOF 退出）。
/// 冷路径由 `handle_gui` 在 token 握手后调用；热路径由 `handle_gui_warm` 在被领用后调用。
async fn serve_gui(
    entry: Arc<request::RequestEntry>,
    mut reader: Reader,
    gui_tx: tokio::sync::mpsc::UnboundedSender<ServerMsg>,
    writer: tokio::task::JoinHandle<()>,
    state: &Arc<ServerState>,
) {
    entry.gui_connected.store(true, Ordering::SeqCst);
    if let Ok(mut slot) = entry.gui.lock() {
        *slot = Some(gui_tx.clone());
    }

    // 下发题目。
    let _ = gui_tx.send(request::show_msg(&entry));

    // 方案5(b)：若调用方 agent 已异步解析完成（walk 早于本连接），握手即补发 AgentResolved
    // （覆盖「解析早于 helper 连接」竞态；解析晚于连接的情形由 spawn_agent_resolve 自行推送）。
    if let Some(r) = entry.resolved_agent.lock().unwrap().clone() {
        let _ = gui_tx.send(ServerMsg::AgentResolved {
            kind: r.kind,
            pid: r.pid,
        });
    }

    // 握手即带上当前自更新态（有更新 / 待生效），使弹窗一打开就知道，无需等下次广播。
    {
        let u = state.update.lock().unwrap();
        if u.available || u.pending {
            let _ = gui_tx.send(ServerMsg::UpdateState {
                available: u.available,
                latest_version: u.latest_version.clone(),
                pending: u.pending,
            });
        }
    }

    // Keep the socket alive after a terminal decision until the native window confirms dismissal.
    // This prevents the next popup from taking focus while the previous window is still closing.
    let dismiss_timeout = tokio::time::sleep(Duration::from_secs(24 * 60 * 60));
    tokio::pin!(dismiss_timeout);
    let mut awaiting_dismissal = false;
    let mut answer_received = false;
    let mut dismissal_received = false;
    loop {
        tokio::select! {
            msg = ipc::read_msg::<_, ClientMsg>(&mut reader) => {
                match msg {
                    Ok(Some(ClientMsg::PopupReady {
                        request_id,
                        window_number,
                    })) if request_id == entry.request_id => {
                        entry.gui_ready.store(true, Ordering::SeqCst);
                        update_popup_focus(state, |focus| {
                            focus.ready(&request_id, ReadyMetadata { window_number })
                        });
                    }
                    Ok(Some(ClientMsg::PopupFocused { request_id }))
                        if request_id == entry.request_id =>
                    {
                        update_popup_focus(state, |focus| focus.claim(&request_id));
                    }
                    Ok(Some(ClientMsg::PopupDismissed { request_id }))
                        if request_id == entry.request_id =>
                    {
                        update_popup_focus(state, |focus| focus.dismissed(&request_id));
                        if !answer_received {
                            entry.coordinator.submit(ChannelResult::cancel("popup"));
                        }
                        dismissal_received = true;
                        break;
                    }
                    Ok(Some(ClientMsg::Answer { action, answers, .. })) => {
                        if answer_received {
                            continue;
                        }
                        let result = match action {
                            ChannelAction::Send => ChannelResult {
                                action: ChannelAction::Send,
                                answers,
                                source_channel_id: "popup".to_string(),
                            },
                            ChannelAction::Cancel => ChannelResult::cancel("popup"),
                        };
                        entry.coordinator.submit(result);
                        answer_received = true;
                        awaiting_dismissal = true;
                        dismiss_timeout.as_mut().reset(
                            tokio::time::Instant::now() + Duration::from_millis(750),
                        );
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        // Helper 断开且未作答：视为取消（已完成则为 no-op）。
                        if !answer_received {
                            entry.coordinator.submit(ChannelResult::cancel("popup"));
                        }
                        break;
                    }
                }
            }
            _ = entry.cancel.notified(), if !awaiting_dismissal => {
                awaiting_dismissal = true;
                dismiss_timeout.as_mut().reset(
                    tokio::time::Instant::now() + Duration::from_millis(750),
                );
            }
            _ = &mut dismiss_timeout, if awaiting_dismissal => break,
        }
    }

    // 收尾：清空发送端槽位并丢弃发送端 → 写任务结束 → GUI 连接关闭 → Helper 收到 EOF 退出。
    if let Ok(mut slot) = entry.gui.lock() {
        *slot = None;
    }
    drop(gui_tx);
    let _ = writer.await;
    if !dismissal_received {
        update_popup_focus(state, |focus| focus.disconnected(&entry.request_id));
    }
}

/// Structured confirmation popup service. A helper disconnect is a channel failure, never a
/// synthetic denial. Readiness is accepted only after the frontend reports its first paint.
async fn serve_confirm_gui(
    entry: Arc<request::ConfirmEntry>,
    mut reader: Reader,
    gui_tx: tokio::sync::mpsc::UnboundedSender<ServerMsg>,
    writer: tokio::task::JoinHandle<()>,
    state: &Arc<ServerState>,
) {
    entry.gui_connected.store(true, Ordering::SeqCst);
    if let Ok(mut slot) = entry.gui.lock() {
        *slot = Some(gui_tx.clone());
    }
    let _ = gui_tx.send(ServerMsg::Show(entry.show.clone()));

    {
        let update = state.update.lock().unwrap();
        if update.available || update.pending {
            let _ = gui_tx.send(ServerMsg::UpdateState {
                available: update.available,
                latest_version: update.latest_version.clone(),
                pending: update.pending,
            });
        }
    }

    let mut failed = false;
    let dismiss_timeout = tokio::time::sleep(Duration::from_secs(24 * 60 * 60));
    tokio::pin!(dismiss_timeout);
    let mut awaiting_dismissal = false;
    let mut answer_received = false;
    let mut dismissal_received = false;
    loop {
        tokio::select! {
            msg = ipc::read_msg::<_, ClientMsg>(&mut reader) => {
                match msg {
                    Ok(Some(ClientMsg::PopupReady {
                        request_id,
                        window_number,
                    })) if request_id == entry.request_id => {
                        update_popup_focus(state, |focus| {
                            focus.ready(&request_id, ReadyMetadata { window_number })
                        });
                    }
                    Ok(Some(ClientMsg::PopupFocused { request_id }))
                        if request_id == entry.request_id =>
                    {
                        update_popup_focus(state, |focus| focus.claim(&request_id));
                    }
                    Ok(Some(ClientMsg::PopupDismissed { request_id }))
                        if request_id == entry.request_id =>
                    {
                        update_popup_focus(state, |focus| focus.dismissed(&request_id));
                        failed = !answer_received && !awaiting_dismissal;
                        dismissal_received = true;
                        break;
                    }
                    Ok(Some(ClientMsg::ConfirmReady { request_id }))
                        if request_id == entry.request_id =>
                    {
                        if !entry.mark_ready("popup", String::new()) {
                            failed = true;
                            break;
                        }
                    }
                    Ok(Some(ClientMsg::ConfirmAnswer {
                        request_id,
                        choice_index,
                        comment,
                    })) if request_id == entry.request_id => {
                        if !entry.is_ready("popup") {
                            log(&format!(
                                "confirmation answer arrived before popup ready for {}",
                                entry.request_id
                            ));
                            continue;
                        }
                        match entry.coordinator.submit_wire(
                            choice_index,
                            comment,
                            "popup",
                        ) {
                            Ok(_) => {
                                answer_received = true;
                                awaiting_dismissal = true;
                                dismiss_timeout.as_mut().reset(
                                    tokio::time::Instant::now() + Duration::from_millis(750),
                                );
                            }
                            Err(error) => log(&format!(
                                "invalid popup confirmation answer for {}: {error}",
                                entry.request_id
                            )),
                        }
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        failed = !answer_received && !awaiting_dismissal;
                        break;
                    }
                }
            }
            _ = entry.cancel.notified(), if !awaiting_dismissal => {
                awaiting_dismissal = true;
                dismiss_timeout.as_mut().reset(
                    tokio::time::Instant::now() + Duration::from_millis(750),
                );
            }
            _ = &mut dismiss_timeout, if awaiting_dismissal => break,
        }
    }

    if let Ok(mut slot) = entry.gui.lock() {
        *slot = None;
    }
    if failed && entry.mark_failed("popup", "popup helper disconnected") {
        entry
            .coordinator
            .fallback(ConfirmFallbackReason::NoAvailableChannel);
    }
    drop(gui_tx);
    let _ = writer.await;
    if !dismissal_received {
        update_popup_focus(state, |focus| focus.disconnected(&entry.request_id));
    }
}

/// 方案6 预热弹窗连接（`--popup --warm` 拉起的进程 → `GuiWarmReady`）：建专用写端、入热池待命，
/// 等二选一——① `dispatch_popup` 领用并经 `assign` 交来请求 entry → 进入 `serve_gui`；
/// ② 连接 EOF/holder 死亡 → 清池 + 触发补热。**非保活**（不计入 `active`，同托盘订阅）。
async fn handle_gui_warm(mut reader: Reader, w: OwnedWriteHalf, state: &Arc<ServerState>) {
    // 非保活：抵消 `handle_conn` 入口的 active+1，使「仅有待命热实例」时 daemon 仍可空闲退出。
    state.active.fetch_sub(1, Ordering::SeqCst);

    let (gui_tx, mut gui_rx) = tokio::sync::mpsc::unbounded_channel::<ServerMsg>();
    let mut w = w;
    let writer = tokio::spawn(async move {
        while let Some(msg) = gui_rx.recv().await {
            if ipc::write_msg(&mut w, &msg).await.is_err() {
                break;
            }
        }
    });

    // 入池（恒 ≤1）：池已占说明已有热实例 → 本连接多余，直接关闭退出。
    // 注意：MutexGuard 非 Send，绝不能跨 await 持有 → 用紧作用域块只算出 `occupied` 布尔，再在块外 await。
    let (assign_tx, assign_rx) = tokio::sync::oneshot::channel::<InteractionEntry>();
    let occupied = {
        let mut pool = state.warm_pool.lock().unwrap();
        if pool.is_some() {
            true
        } else {
            *pool = Some(WarmSlot {
                assign: assign_tx,
                gui_tx: gui_tx.clone(),
            });
            state.warm_spawning.store(false, Ordering::SeqCst);
            false
        }
    };
    if occupied {
        drop(gui_tx);
        let _ = writer.await;
        state.active.fetch_add(1, Ordering::SeqCst);
        return;
    }
    log("warm popup ready; standing by");

    // 待命：等领用（biased 优先，避免与 EOF 竞态丢失领用）或连接断开（热进程死亡）。
    let assigned = tokio::select! {
        biased;
        a = assign_rx => a.ok(),
        _ = wait_cli_eof(&mut reader) => None,
    };

    match assigned {
        // 被领用：与冷路径同尾段（下发 show + 读应答）。
        Some(entry) => match entry {
            InteractionEntry::Ask(entry) => serve_gui(entry, reader, gui_tx, writer, state).await,
            InteractionEntry::Confirm(entry) => {
                serve_confirm_gui(entry, reader, gui_tx, writer, state).await
            }
        },
        // 热进程死亡 / 被回收：若池中仍是本槽则清空。
        None => {
            {
                let mut pool = state.warm_pool.lock().unwrap();
                if pool
                    .as_ref()
                    .map(|s| s.gui_tx.same_channel(&gui_tx))
                    .unwrap_or(false)
                {
                    *pool = None;
                }
            }
            drop(gui_tx);
            let _ = writer.await;
        }
    }

    state.active.fetch_add(1, Ordering::SeqCst);
    // 领用 / 死亡后维持池满（self-gated：关 / 无显示 / draining 则不补）。
    maybe_topup_warm(state);
}

/// 菜单栏图标是否在当前平台被支持（用于 daemon 决定是否兜底拉起宿主）。
/// macOS 恒真；Linux 仅在存在图形会话时（保守门控，headless 不拉宿主）。
fn tray_supported() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
}

/// 按配置兜底拉起 GUI 宿主（spec D14）：`menu_bar_icon != off` 且托盘可用时尝试 spawn。
/// 单实例由宿主自身 flock 去重；always 主要靠登录项，此处为兜底。失败静默。
fn maybe_spawn_gui_host(config: &AppConfig) {
    use crate::config::MenuBarIconMode;
    if config.general.menu_bar_icon == MenuBarIconMode::Off {
        return;
    }
    if !tray_supported() {
        return;
    }
    if let Err(e) = crate::gui_host::spawn_detached() {
        log(&format!("failed to spawn gui-host: {}", e));
    }
}

/// 等待 CLI 提交连接断开（提交后 CLI 不再发消息；任何 EOF/错误即视为断开）。
async fn wait_cli_eof(reader: &mut Reader) {
    loop {
        match ipc::read_msg::<_, ClientMsg>(reader).await {
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return,
        }
    }
}

/// 看门狗：限定时间内 GUI 未连上 → 经渲染通道送「弹窗拉起失败」结果（退出码 3）。
/// `im_attached` 为真时弹窗未连上不判失败——IM 渠道仍可作答。
fn spawn_gui_watchdog(
    entry: Arc<RequestEntry>,
    lang: Lang,
    im_attached: bool,
    state: Arc<ServerState>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(request::GUI_CONNECT_TIMEOUT_SECS)).await;
        if !entry.gui_ready.load(Ordering::SeqCst) {
            update_popup_focus(&state, |focus| focus.dispatch_failed(&entry.request_id));
            if !im_attached {
                let _ = state.registry.send_to_gui(
                    &entry.request_id,
                    ServerMsg::Cancel {
                        request_id: entry.request_id.clone(),
                        winner: "system".to_string(),
                    },
                );
                let _ = entry.final_tx.send(request::popup_failed_outcome(lang));
            }
        }
    });
}

fn spawn_confirm_popup_watchdog(entry: Arc<request::ConfirmEntry>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(request::GUI_CONNECT_TIMEOUT_SECS)).await;
        if entry.mark_starting_failed("popup", "popup did not become ready in time") {
            entry
                .coordinator
                .fallback(ConfirmFallbackReason::NoAvailableChannel);
            entry.cancel.notify_waiters();
        }
    });
}

/// 取得（必要时惰性建连）钉钉 Router；连接已死则重连。失败返回 None。
async fn ensure_dd_router(
    state: &Arc<ServerState>,
    client_id: &str,
    client_secret: &str,
) -> Option<Arc<DdRouter>> {
    let mut guard = state.dd_router.lock().await;
    if let Some(r) = guard.as_ref() {
        if r.is_alive() {
            return Some(r.clone());
        }
    }
    match DdRouter::connect(client_id, client_secret).await {
        Ok(r) => {
            log("dingtalk router connected");
            crate::channels::health::clear("dingding");
            *guard = Some(r.clone());
            Some(r)
        }
        Err(e) => {
            log(&format!("dingtalk router connect failed: {}", e));
            crate::channels::health::report("dingding", e);
            *guard = None;
            None
        }
    }
}

/// 取得（必要时惰性建连）飞书 Router；连接已死则重连。失败返回 None。
async fn ensure_fs_router(
    state: &Arc<ServerState>,
    cfg: &crate::config::FeishuChannelConfig,
) -> Option<Arc<FsRouter>> {
    let mut guard = state.fs_router.lock().await;
    if let Some(r) = guard.as_ref() {
        if r.is_alive() {
            return Some(r.clone());
        }
    }
    match FsRouter::connect(cfg).await {
        Ok(r) => {
            log("feishu router connected");
            crate::channels::health::clear("feishu");
            *guard = Some(r.clone());
            Some(r)
        }
        Err(e) => {
            log(&format!("feishu router connect failed: {}", e));
            crate::channels::health::report("feishu", e);
            *guard = None;
            None
        }
    }
}

/// 取得（必要时惰性建连）Telegram Router；轮询器已停则重建。失败返回 None。
async fn ensure_tg_router(
    state: &Arc<ServerState>,
    cfg: &crate::config::TelegramChannelConfig,
) -> Option<Arc<TgRouter>> {
    let mut guard = state.tg_router.lock().await;
    if let Some(r) = guard.as_ref() {
        if r.is_alive() {
            return Some(r.clone());
        }
    }
    match TgRouter::connect(cfg).await {
        Ok(r) => {
            log("telegram router connected");
            *guard = Some(r.clone());
            Some(r)
        }
        Err(e) => {
            log(&format!("telegram router connect failed: {}", e));
            crate::channels::health::report("telegram", e);
            *guard = None;
            None
        }
    }
}

/// 取得（必要时惰性建连）Slack Router；连接已死则重连。失败返回 None。
async fn ensure_sl_router(
    state: &Arc<ServerState>,
    cfg: &crate::config::SlackChannelConfig,
) -> Option<Arc<SlRouter>> {
    let mut guard = state.sl_router.lock().await;
    if let Some(r) = guard.as_ref() {
        if r.is_alive() {
            return Some(r.clone());
        }
    }
    match SlRouter::connect(cfg).await {
        Ok(r) => {
            log("slack router connected");
            crate::channels::health::clear("slack");
            *guard = Some(r.clone());
            Some(r)
        }
        Err(e) => {
            log(&format!("slack router connect failed: {}", e));
            crate::channels::health::report("slack", e);
            *guard = None;
            None
        }
    }
}

/// 按当前配置把可用 IM 渠道挂到请求协调器上（与弹窗并行抢答）。返回是否至少挂上一个。
/// 失败的渠道经 `Warn` 流给 CLI stderr，并记 daemon.log。
/// 方案5(b)：accept 后**异步**从 `caller_pid` 向上 walk 进程树解析调用方 agent（家族 + pid），完成后
/// 补刷注册表活动（补 pid / env 判不出家族的 MCP 兜底）并把结果存入 `entry` + 经 `AgentResolved`
/// 后推弹窗 badge。`kind_env` 为 CLI 经 env 已判出的家族（None=未判出，典型 MCP `env_clear` → `walk_any`
/// 兜底）。`ps` 游走是阻塞调用，放 blocking 线程；`caller_pid==0`（旧 CLI 不带）则跳过。
/// 整段不阻塞请求关键路径（弹窗已先 spawn）。
#[allow(clippy::too_many_arguments)] // args mirror the submit-time hints; grouping adds churn without clarity
fn spawn_agent_resolve(
    entry: Arc<RequestEntry>,
    state: Arc<ServerState>,
    caller_pid: u32,
    kind_env: Option<AgentKind>,
    sid_env: Option<String>,
    from_mcp: bool,
    auto: bool,
    cwd: Option<String>,
) -> Option<tokio::sync::oneshot::Receiver<Option<AgentKind>>> {
    if caller_pid == 0 {
        return None;
    }
    let (resolved_tx, resolved_rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let resolved = tokio::task::spawn_blocking(move || match kind_env {
            Some(kind) => {
                crate::agents::detect::walk_agent_pid(kind, caller_pid).map(|pid| (kind, pid))
            }
            // env 判不出家族（典型 MCP，env_clear）：进程树兜底找任意家族 agent 祖先。
            None if from_mcp => crate::agents::detect::walk_any_agent(caller_pid),
            None => None,
        })
        .await
        .ok()
        .flatten();

        // Unblock IM title construction before the remaining registry/history/Popup side effects.
        let _ = resolved_tx.send(resolved.as_ref().map(|(kind, _)| *kind));

        let Some((kind, pid)) = resolved else {
            return;
        };

        // 注册表活动：env 有会话 → 按 session 刷新并补 pid；否则（MCP）按 pid 刷新（只更新、绝不新建）。
        let changed = match (kind_env, &sid_env) {
            (Some(k), Some(sid)) => {
                if auto && !from_mcp {
                    state.agents.upsert_working(k, sid, Some(pid), cwd.clone())
                } else {
                    state.agents.touch_activity(k, sid, Some(pid))
                }
            }
            _ => state.agents.touch_activity_by_pid(kind, pid),
        };
        if changed {
            state.agents.persist();
            broadcast_agents_state(&state);
        }

        // 回填协调器的 agent 家族（MCP 场景 env 判不出，历史记录只有这条路能拿到）。
        entry.coordinator.set_agent_kind(kind.as_str().to_string());

        // 存入 entry + 后推弹窗（helper 已连上则即送；未连则握手时由 handle_gui 补发，覆盖竞态）。
        let resolved = request::ResolvedAgent {
            kind: Some(kind.as_str().to_string()),
            pid: Some(pid),
        };
        if let Ok(mut slot) = entry.resolved_agent.lock() {
            *slot = Some(resolved.clone());
        }
        if let Ok(slot) = entry.gui.lock() {
            if let Some(tx) = slot.as_ref() {
                let _ = tx.send(ServerMsg::AgentResolved {
                    kind: resolved.kind,
                    pid: resolved.pid,
                });
            }
        }
    });
    Some(resolved_rx)
}

async fn agent_kind_for_im(
    known: Option<AgentKind>,
    from_mcp: bool,
    resolution: Option<tokio::sync::oneshot::Receiver<Option<AgentKind>>>,
) -> Option<AgentKind> {
    if known.is_some() || !from_mcp {
        return known;
    }
    let receiver = resolution?;
    match tokio::time::timeout(Duration::from_millis(IM_AGENT_RESOLVE_WAIT_MS), receiver).await {
        Ok(Ok(kind)) => kind,
        Ok(Err(_)) | Err(_) => None,
    }
}

/// 是否启用了任一 IM 渠道（仅看非密钥的 `enabled` 标志）。用于方案4 在读钥匙串前的廉价门控：
/// 可安全用 `load_without_secrets()` 的结果判定（`enabled` 不是密钥）。
fn any_im_enabled(config: &AppConfig) -> bool {
    let ch = &config.channels;
    ch.dingding.enabled || ch.feishu.enabled || ch.telegram.enabled || ch.slack.enabled
}

/// 当前配置完整、可实际投递的 IM 渠道，顺序与既有投递顺序一致。
fn available_im_channels(config: &AppConfig) -> Vec<&'static str> {
    let mut available = Vec::new();
    if crate::app::is_dingding_active(config) {
        available.push("dingding");
    }
    if crate::app::is_feishu_active(config) {
        available.push("feishu");
    }
    if crate::app::is_telegram_active(config) {
        available.push("telegram");
    }
    if crate::app::is_slack_active(config) {
        available.push("slack");
    }
    available
}

/// 按需发送的统一候选规则：优先活跃槽 ∪ watch；若 Popup 不可用且交集为空，
/// 兜底所有可用 IM，保证不会因失效活跃槽造成零投递。
fn select_im_delivery_candidates(
    auto_activation: bool,
    available: &[&'static str],
    active: Option<&str>,
    watching: &[String],
    popup_available: bool,
) -> Vec<&'static str> {
    if !auto_activation {
        return available.to_vec();
    }
    let selected: Vec<&'static str> = available
        .iter()
        .copied()
        .filter(|id| active == Some(*id) || watching.iter().any(|watched| watched.as_str() == *id))
        .collect();
    if selected.is_empty() && !popup_available {
        available.to_vec()
    } else {
        selected
    }
}

fn im_conversation_origin(
    entry: &RequestEntry,
    resolved_hint: Option<AgentKind>,
) -> crate::channels::ConversationOrigin {
    let resolved = entry
        .resolved_agent
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().and_then(|agent| agent.kind.clone()))
        .and_then(|kind| AgentKind::parse(&kind));
    let initial = entry.show.agent_kind.as_deref().and_then(AgentKind::parse);
    let kind = resolved_hint.or(resolved).or(initial);
    crate::channels::ConversationOrigin::new(
        &entry.show.source,
        kind.map(AgentKind::as_str),
        &entry.show.project,
    )
}

#[allow(clippy::too_many_arguments)] // arguments mirror the request handoff context
async fn attach_im_channels(
    entry: &Arc<RequestEntry>,
    state: &Arc<ServerState>,
    w: &mut OwnedWriteHalf,
    lang: Lang,
    popup_available: bool,
    known_agent_kind: Option<AgentKind>,
    from_mcp: bool,
    agent_resolution: Option<tokio::sync::oneshot::Receiver<Option<AgentKind>>>,
) -> bool {
    // 方案4（spec §4）的「仅弹窗用户零钥匙串」目标现由缓存快照达成：快照在启动 / 配置变更时
    // 已解析密钥，这里读缓存即可，热路径不再触碰钥匙串与磁盘。
    let config = state.config_snapshot();
    if !any_im_enabled(&config) {
        return false;
    }
    let request = entry.request().clone();
    let sink = entry.coordinator.clone();
    let mut attached = false;

    // 「IM 会话期自动激活」：开关开时，投放渠道 = 当前有效活跃槽 ∪ 正在 watch 本次调用方
    // agent 的渠道。若 Popup 不可用且候选为空，则全发可用 IM 作为可达性兜底。其余情况下，
    // 非候选 IM 由入站监听器保持连接、只监听 here，不发卡片。开关关时维持旧「全发」行为。
    let active = state.active_channel.lock().unwrap().clone();
    let watching: Vec<String> = match entry.agent_session_id.as_ref() {
        Some(sid) => state
            .watch
            .subs
            .lock()
            .unwrap()
            .iter()
            .filter(|s| &s.session_id == sid && !s.rewatchable)
            .map(|s| s.channel.clone())
            .collect(),
        None => Vec::new(),
    };
    let available = available_im_channels(&config);
    let candidates = select_im_delivery_candidates(
        config.channels.auto_activation,
        &available,
        active.as_deref(),
        &watching,
        popup_available,
    );
    if candidates.is_empty() {
        return false;
    }
    // MCP env_clear means the CLI cannot name its Agent. Wait only when this request will actually
    // deliver to IM; Popup has already been dispatched and remains independent of this budget.
    let agent_kind = agent_kind_for_im(known_agent_kind, from_mcp, agent_resolution).await;

    if candidates.contains(&"dingding") {
        let dd = &config.channels.dingding;
        match ensure_dd_router(state, dd.client_id.trim(), dd.client_secret.trim()).await {
            Some(router) => {
                let ch: Arc<dyn Channel> = Arc::new(DingTalkChannel::shared(dd.clone(), router));
                entry.coordinator.register(ch.clone());
                let origin = im_conversation_origin(entry, agent_kind);
                ch.start(&request, &origin, sink.clone());
                attached = true;
            }
            None => {
                let _ = ipc::write_msg(
                    w,
                    &ServerMsg::Warn {
                        text: format!(
                            "{}{}",
                            crate::i18n::warn_prefix(lang),
                            crate::i18n::tr(lang, "channel.ddConfigInvalidSkip")
                                .replace("{e}", "Stream connection failed"),
                        ),
                    },
                )
                .await;
            }
        }
    }

    if candidates.contains(&"feishu") {
        let fs = &config.channels.feishu;
        match ensure_fs_router(state, fs).await {
            Some(router) => {
                let ch: Arc<dyn Channel> = Arc::new(FeishuChannel::shared(fs.clone(), router));
                entry.coordinator.register(ch.clone());
                let origin = im_conversation_origin(entry, agent_kind);
                ch.start(&request, &origin, sink.clone());
                attached = true;
            }
            None => {
                let _ = ipc::write_msg(
                    w,
                    &ServerMsg::Warn {
                        text: format!(
                            "{}{}",
                            crate::i18n::warn_prefix(lang),
                            crate::i18n::tr(lang, "channel.fsConfigInvalidSkip")
                                .replace("{e}", "WebSocket connection failed"),
                        ),
                    },
                )
                .await;
            }
        }
    }

    if candidates.contains(&"telegram") {
        let tg = &config.channels.telegram;
        match ensure_tg_router(state, tg).await {
            Some(router) => {
                let ch: Arc<dyn Channel> = Arc::new(TelegramChannel::shared(tg.clone(), router));
                entry.coordinator.register(ch.clone());
                let origin = im_conversation_origin(entry, agent_kind);
                ch.start(&request, &origin, sink.clone());
                attached = true;
            }
            None => {
                let _ = ipc::write_msg(
                    w,
                    &ServerMsg::Warn {
                        text: format!(
                            "{}{}",
                            crate::i18n::warn_prefix(lang),
                            crate::i18n::tr(lang, "channel.tgConfigInvalidSkip")
                                .replace("{e}", "poller start failed"),
                        ),
                    },
                )
                .await;
            }
        }
    }

    if candidates.contains(&"slack") {
        let sl = &config.channels.slack;
        match ensure_sl_router(state, sl).await {
            Some(router) => {
                let ch: Arc<dyn Channel> = Arc::new(SlackChannel::shared(sl.clone(), router));
                entry.coordinator.register(ch.clone());
                let origin = im_conversation_origin(entry, agent_kind);
                ch.start(&request, &origin, sink.clone());
                attached = true;
            }
            None => {
                let _ = ipc::write_msg(
                    w,
                    &ServerMsg::Warn {
                        text: format!(
                            "{}{}",
                            crate::i18n::warn_prefix(lang),
                            crate::i18n::tr(lang, "channel.slConfigInvalidSkip")
                                .replace("{e}", "Socket Mode connection failed"),
                        ),
                    },
                )
                .await;
            }
        }
    }

    attached
}

fn confirm_im_candidates(
    entry: &Arc<request::ConfirmEntry>,
    state: &Arc<ServerState>,
    config: &AppConfig,
    popup_available: bool,
) -> Vec<&'static str> {
    let active = state.active_channel.lock().unwrap().clone();
    let watching: Vec<String> = state
        .watch
        .subs
        .lock()
        .unwrap()
        .iter()
        .filter(|watch| watch.session_id == entry.agent_session_id && !watch.rewatchable)
        .map(|watch| watch.channel.clone())
        .collect();
    let available = available_im_channels(config);
    select_im_delivery_candidates(
        config.channels.auto_activation,
        &available,
        active.as_deref(),
        &watching,
        popup_available,
    )
}

async fn attach_confirm_im_channels(
    entry: &Arc<request::ConfirmEntry>,
    state: &Arc<ServerState>,
    config: &AppConfig,
    candidates: &[&str],
) {
    for channel in candidates {
        match *channel {
            "feishu" => match ensure_fs_router(state, &config.channels.feishu).await {
                Some(router) => crate::channels::confirm::start_feishu(
                    entry.clone(),
                    config.channels.feishu.clone(),
                    router,
                ),
                None => {
                    if entry.mark_failed("feishu", "Feishu router unavailable") {
                        entry
                            .coordinator
                            .fallback(ConfirmFallbackReason::NoAvailableChannel);
                    }
                }
            },
            "telegram" => match ensure_tg_router(state, &config.channels.telegram).await {
                Some(router) => crate::channels::confirm::start_telegram(
                    entry.clone(),
                    config.channels.telegram.clone(),
                    router,
                ),
                None => {
                    if entry.mark_failed("telegram", "Telegram router unavailable") {
                        entry
                            .coordinator
                            .fallback(ConfirmFallbackReason::NoAvailableChannel);
                    }
                }
            },
            "slack" => match ensure_sl_router(state, &config.channels.slack).await {
                Some(router) => crate::channels::confirm::start_slack(
                    entry.clone(),
                    config.channels.slack.clone(),
                    router,
                ),
                None => {
                    if entry.mark_failed("slack", "Slack router unavailable") {
                        entry
                            .coordinator
                            .fallback(ConfirmFallbackReason::NoAvailableChannel);
                    }
                }
            },
            "dingding" => match ensure_dd_router(
                state,
                config.channels.dingding.client_id.trim(),
                config.channels.dingding.client_secret.trim(),
            )
            .await
            {
                Some(router) => crate::channels::confirm::start_dingtalk(
                    entry.clone(),
                    config.channels.dingding.clone(),
                    router,
                ),
                None => {
                    if entry.mark_failed("dingding", "DingTalk router unavailable") {
                        entry
                            .coordinator
                            .fallback(ConfirmFallbackReason::NoAvailableChannel);
                    }
                }
            },
            _ => {}
        }
    }
}

/// 通用入站监听循环（与渠道无关）：从原始消息流抽取 (发送者, 文本)，按期望发送者过滤后交 `handle_inbound`。
/// 收到 `stop`（改配置时由 `invalidate_changed_routers` 触发）或流结束（连接断开）即退出，
/// 退出时**按身份**释放监听位（不误删改配置后新建监听的认领），下次提问 / 配置变更可重建。
/// 一条经身份过滤前的入站消息（抽取器输出）。`text` 为 `None` 表示来自发送者的**非文本**消息
/// （图片/文件等）——文本走命令/引导分派，非文本仅在「无在途提问」时回引导（见 `handle_inbound`）。
struct Inbound {
    sender: String,
    text: Option<String>,
}

/// watch 卡的渠道传输：发送 / 就地编辑一张实时状态卡，屏蔽各渠道 API 与标记语言差异。
/// 渲染入口各渠道自备（飞书 `watch::card_view`+`build_watch_card`、Telegram HTML、
/// Slack Block Kit），消息 id 编码见 `WatchEntry::message_id`。
enum WatchClient {
    Feishu(crate::feishu::client::FeishuClient),
    Telegram(crate::telegram::TelegramClient),
    Slack {
        client: crate::slack::client::SlackClient,
        /// DM 频道 id（`conversations.open` 解析；同一 user 稳定）。
        dm: String,
    },
    /// 钉钉互动卡片高级版：专用 watch 模板 + 变量更新（`dingtalk/watch.rs`）。
    DingTalk(crate::dingtalk::client::DingTalkClient),
}

/// 取渠道的缓存 [`WatchClient`]（无则构建并缓存）。构建要新建 reqwest 连接池，Slack 还含
/// `open_dm` 网络调用——watch tick 每拍逐渠道重建既慢又浪费。缓存后跨拍复用（TLS keep-alive），
/// 配置变更时整体失效重建；渠道不可用 → None（不缓存，下一拍重试）。
async fn watch_client(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
) -> Option<Arc<WatchClient>> {
    if let Some(hit) = state.watch.clients.lock().unwrap().get(channel_id) {
        return Some(hit.clone());
    }
    let built = Arc::new(WatchClient::for_channel(channel_id, config).await?);
    state
        .watch
        .clients
        .lock()
        .unwrap()
        .insert(channel_id.to_string(), built.clone());
    Some(built)
}

impl WatchClient {
    /// 按渠道构造。渠道未配置/不可用 → None（该渠道订阅本拍跳过，下一拍重试）。
    async fn for_channel(channel_id: &str, config: &AppConfig) -> Option<WatchClient> {
        match channel_id {
            "feishu" => crate::feishu::client::FeishuClient::new(&config.channels.feishu)
                .ok()
                .map(WatchClient::Feishu),
            "telegram" => {
                let tg = &config.channels.telegram;
                crate::telegram::TelegramClient::new(
                    tg.bot_token.clone(),
                    tg.chat_id.clone(),
                    tg.api_base_url.clone(),
                )
                .ok()
                .map(WatchClient::Telegram)
            }
            "slack" => {
                let client = crate::slack::client::SlackClient::new(&config.channels.slack).ok()?;
                let dm = client.open_dm().await.ok()?;
                Some(WatchClient::Slack { client, dm })
            }
            "dingding" => crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
                .ok()
                .map(WatchClient::DingTalk),
            _ => None,
        }
    }

    /// 每卡最短编辑间隔（毫秒）。Slack `chat.update` 限频更紧（Tier 3 ≈50/min），取 2s
    /// （计划 R5 定案；签名门控使实际编辑远稀于理论上限）；飞书 PATCH / Telegram
    /// editMessageText / 钉钉实例更新（PoC 实测 2s×150 连发零频控，p50 ≈60–95ms）取 1s。
    fn min_edit_interval_ms(&self) -> u64 {
        match self {
            WatchClient::Slack { .. } => 2000,
            _ => 1000,
        }
    }

    /// 发送一张新卡，返回消息 id（编码见 `WatchEntry::message_id`）。
    async fn send(
        &self,
        frame: &crate::watch::WatchFrame,
        mode: crate::watch::CardMode,
        now: u64,
        lang: Lang,
    ) -> Result<String, String> {
        match self {
            WatchClient::Feishu(c) => {
                let card = crate::feishu::card::build_watch_card(&crate::watch::card_view(
                    frame, mode, now, lang, None,
                ));
                c.send_card(&card).await.map_err(|e| e.to_string())
            }
            WatchClient::Telegram(c) => {
                // 先按 mode 取 markup（借用），再把 mode 交给渲染（CardMode 非 Copy）。
                let markup = matches!(mode, crate::watch::CardMode::Active)
                    .then(|| crate::telegram::watch::inline_keyboard(lang));
                let html = crate::telegram::watch::render_watch_html(frame, mode, now, lang);
                c.send_message(&html, Some("HTML"), markup)
                    .await
                    .map(|mid| mid.to_string())
                    .map_err(|e| e.to_string())
            }
            WatchClient::Slack { client, dm } => {
                let (blocks, fallback) =
                    crate::slack::watch::build_watch_blocks(frame, mode, now, lang, None);
                client
                    .post_message(dm, Some(&blocks), &fallback)
                    .await
                    .map_err(|e| e.to_string())
            }
            WatchClient::DingTalk(c) => {
                // 消息 id = 自铸 outTrackId（钉钉创建卡片由调用方指定实例 id，天然可编辑）。
                let otid = format!("watch-{}", uuid::Uuid::new_v4());
                let map = crate::dingtalk::watch::build_watch_param_map(frame, mode, now, lang);
                c.create_and_deliver_card(
                    &otid,
                    crate::dingtalk::watch::DEFAULT_WATCH_CARD_TEMPLATE_ID,
                    map,
                    serde_json::json!({}),
                )
                .await
                .map(|()| otid)
                .map_err(|e| e.to_string())
            }
        }
    }

    /// 就地编辑已发出的卡。`session_id`：`AutoStopped` 终态需传入以嵌入重新关注按钮。
    async fn edit(
        &self,
        message_id: &str,
        frame: &crate::watch::WatchFrame,
        mode: crate::watch::CardMode,
        now: u64,
        lang: Lang,
        session_id: Option<&str>,
    ) -> Result<(), String> {
        match self {
            WatchClient::Feishu(c) => {
                let card = crate::feishu::card::build_watch_card(&crate::watch::card_view(
                    frame, mode, now, lang, session_id,
                ));
                c.patch_card(message_id, &card)
                    .await
                    .map_err(|e| e.to_string())
            }
            WatchClient::Telegram(c) => {
                let mid: i64 = message_id
                    .parse()
                    .map_err(|_| "bad message id".to_string())?;
                match (&mode, session_id) {
                    (crate::watch::CardMode::Final(kind), Some(_)) if kind.is_rewatchable() => {
                        let markup = crate::telegram::watch::rewatch_keyboard(kind, lang);
                        let html = crate::telegram::watch::render_watch_html(
                            frame,
                            crate::watch::CardMode::Active,
                            now,
                            lang,
                        );
                        c.edit_message_text(mid, &html, Some("HTML"), Some(markup))
                            .await
                            .map_err(|e| e.to_string())
                    }
                    _ => {
                        let markup = matches!(mode, crate::watch::CardMode::Active)
                            .then(|| crate::telegram::watch::inline_keyboard(lang));
                        let html =
                            crate::telegram::watch::render_watch_html(frame, mode, now, lang);
                        c.edit_message_text(mid, &html, Some("HTML"), markup)
                            .await
                            .map_err(|e| e.to_string())
                    }
                }
            }
            WatchClient::Slack { client, dm } => {
                let (blocks, fallback) =
                    crate::slack::watch::build_watch_blocks(frame, mode, now, lang, session_id);
                client
                    .update_message(dm, message_id, Some(&blocks), &fallback)
                    .await
                    .map_err(|e| e.to_string())
            }
            WatchClient::DingTalk(c) => {
                let map = crate::dingtalk::watch::build_watch_param_map(frame, mode, now, lang);
                c.update_card_private(message_id, map, serde_json::json!({}))
                    .await
                    .map_err(|e| e.to_string())
            }
        }
    }
}

/// watch 按钮语义（渠道无关）。
enum WatchBtn {
    Unwatch,
    Refresh,
}

/// 当前已建连且存活的 IM 长连接名（供 `daemon status` 展示）。
async fn active_im_connections(state: &Arc<ServerState>) -> Vec<String> {
    let mut v = Vec::new();
    if state
        .dd_router
        .lock()
        .await
        .as_ref()
        .map(|r| r.is_alive())
        .unwrap_or(false)
    {
        v.push("dingtalk".to_string());
    }
    if state
        .fs_router
        .lock()
        .await
        .as_ref()
        .map(|r| r.is_alive())
        .unwrap_or(false)
    {
        v.push("feishu".to_string());
    }
    if state
        .tg_router
        .lock()
        .await
        .as_ref()
        .map(|r| r.is_alive())
        .unwrap_or(false)
    {
        v.push("telegram".to_string());
    }
    if state
        .sl_router
        .lock()
        .await
        .as_ref()
        .map(|r| r.is_alive())
        .unwrap_or(false)
    {
        v.push("slack".to_string());
    }
    v
}

/// spawn 一个 GUI Helper 进程（`AskHuman --popup --endpoint <sock> --token <tok>`）。
/// `perf_id` 非空时把性能埋点开关 + 关联 id（及可选的 autodismiss）经 env 透传给 helper，
/// 使其 GUI/前端各阶段标记并入同一条时间线。
fn spawn_gui_helper(token: &str, perf_id: &str, perf_autodismiss: bool) -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--popup")
        .arg("--endpoint")
        .arg(transport::socket_path())
        .arg("--token")
        .arg(token)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if !perf_id.is_empty() {
        cmd.env("ASKHUMAN_PERF", "1");
        cmd.env("ASKHUMAN_PERF_ID", perf_id);
        if perf_autodismiss {
            cmd.env("ASKHUMAN_PERF_AUTODISMISS", "1");
        }
    }
    cmd.spawn().map(|_| ())
}

/// 方案6：spawn 一个预热弹窗进程（`--popup --warm`，无 token、无 perf env）。它会建好隐藏窗 + 挂载前端
/// 后发 `GuiWarmReady` 入热池待命。
fn spawn_warm_helper() -> std::io::Result<()> {
    use std::process::{Command, Stdio};
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    cmd.arg("--popup")
        .arg("--warm")
        .arg("--endpoint")
        .arg(transport::socket_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn().map(|_| ())
}

/// Popup 渠道是否应当投放。显示环境显式传入，便于覆盖完整真值表。
fn popup_should_dispatch(config: &AppConfig, display_available: bool) -> bool {
    config.channels.popup.enabled && display_available
}

/// 配置上是否需要预热 Popup。Popup 渠道关闭时不应保留隐藏 WebView。
fn popup_prewarm_requested(config: &AppConfig) -> bool {
    config.channels.popup.enabled && config.general.popup_prewarm
}

/// 弹窗预热是否启用（配置开关，读最近一次配置快照，无磁盘 I/O）。
fn warm_enabled(state: &Arc<ServerState>) -> bool {
    state
        .config
        .lock()
        .map(|c| popup_prewarm_requested(&c))
        .unwrap_or(false)
}

/// 是否有可用显示（§D-M3）：无显示（headless）不预热，零浪费。macOS 恒真（GUI 会话）；
/// Linux 看 `DISPLAY`/`WAYLAND_DISPLAY`。
fn has_display() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
}

/// 方案6 弹窗派发：优先领用热池中的预热弹窗（把请求 entry 交给其 holder 任务，秒级上屏）；
/// 池空 / 开关关 / 无显示 / holder 已死 → 回退冷 spawn。返回弹窗是否成功就绪。
fn dispatch_popup(
    entry: &Arc<request::RequestEntry>,
    state: &Arc<ServerState>,
    perf_id: &str,
    perf_autodismiss: bool,
) -> bool {
    dispatch_interaction_popup(
        InteractionEntry::Ask(entry.clone()),
        state,
        perf_id,
        perf_autodismiss,
    )
}

fn dispatch_interaction_popup(
    entry: InteractionEntry,
    state: &Arc<ServerState>,
    perf_id: &str,
    perf_autodismiss: bool,
) -> bool {
    let request_id = entry.request_id().to_string();
    let seq = entry.seq();
    update_popup_focus(state, |focus| focus.reserve(request_id.clone(), seq));
    // 取出热池槽（恒 ≤1）。
    let slot = state.warm_pool.lock().unwrap().take();
    if let Some(slot) = slot {
        match slot.assign.send(entry.clone()) {
            Ok(()) => {
                crate::perf::mark(perf_id, "dmn.assigned");
                // 领用成功 → 立即补热，维持池满。
                maybe_topup_warm(state);
                return true;
            }
            Err(_) => {
                // holder 已死（罕见竞态）：池已 take 空，触发补热后回退冷路径。
                maybe_topup_warm(state);
            }
        }
    }
    // 冷路径（兜底，完整保留）。
    match spawn_gui_helper(entry.token(), perf_id, perf_autodismiss) {
        Ok(()) => {
            crate::perf::mark(perf_id, "dmn.spawned");
            true
        }
        Err(e) => {
            log(&format!("failed to spawn GUI helper: {}", e));
            update_popup_focus(state, |focus| focus.dispatch_failed(&request_id));
            false
        }
    }
}

/// 方案6 补热（top-up，恒维持 1 个待命热实例）。自门控：开关关 / 无显示 / 排空中 / 池非空 / 已在补热
/// 任一满足则不补。补热进程连上发 `GuiWarmReady` 后由 `handle_gui_warm` 入池并清 `warm_spawning`。
fn maybe_topup_warm(state: &Arc<ServerState>) {
    if !warm_enabled(state) || !has_display() || state.draining.load(Ordering::SeqCst) {
        return;
    }
    if state.warm_pool.lock().unwrap().is_some() {
        return;
    }
    // 去重：已在补热中则跳过（避免并发 spawn 多个热实例）。
    if state.warm_spawning.swap(true, Ordering::SeqCst) {
        return;
    }
    match spawn_warm_helper() {
        Ok(()) => {
            log("preheating popup helper");
            // 兜底：若热进程崩溃于连接前，N 秒后清 `warm_spawning` 允许再次补热。
            let state = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if state.warm_pool.lock().unwrap().is_none() {
                    state.warm_spawning.store(false, Ordering::SeqCst);
                }
            });
        }
        Err(e) => {
            state.warm_spawning.store(false, Ordering::SeqCst);
            log(&format!("failed to preheat popup helper: {}", e));
        }
    }
}

/// 方案6 回收：清空热池槽（drop `assign`/`gui_tx` → holder 的 `assign_rx` 收 Err → 走死亡分支 →
/// drop 其写端 → 热进程收 EOF 自杀）。用于关开关 / 进入排空 / 关停。
fn recycle_warm(state: &Arc<ServerState>) {
    let slot = state.warm_pool.lock().unwrap().take();
    if slot.is_some() {
        log("recycling warm popup helper");
    }
    drop(slot);
    state.warm_spawning.store(false, Ordering::SeqCst);
}

fn cleanup() {
    let _ = std::fs::remove_file(transport::socket_path());
    let _ = std::fs::remove_file(lifecycle::meta_path());
}

/// 临时产物保留时长：消费者(AI)在 CLI 退出后才读图片路径，留足窗口避免误删刚产出的文件。
const TEMP_MAX_AGE: Duration = Duration::from_secs(24 * 3600);

/// 清理 `temp/askhuman/<id>/` 中超过 `TEMP_MAX_AGE` 未改动的目录（A10，启动 + 每小时）。
fn cleanup_temp_dirs() {
    let base = std::env::temp_dir().join("askhuman");
    let Ok(entries) = std::fs::read_dir(&base) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let age = e
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| now.duration_since(t).ok());
        if matches!(age, Some(a) if a >= TEMP_MAX_AGE) {
            let _ = std::fs::remove_dir_all(&p);
        }
    }
}

/// config.json 变更（已去抖）：重载配置 → 失效凭据改动/被禁用渠道的缓存 Router → 通知活动 GUI。
async fn on_config_changed(state: &Arc<ServerState>) {
    let new = AppConfig::load();
    let old = { state.config.lock().unwrap().clone() };
    invalidate_changed_routers(state, &old, &new).await;
    *state.config.lock().unwrap() = new.clone();
    // 缓存的 watch 传输客户端按旧凭据构建，凭据/开关可能已变 → 整体失效，用到时按新配置重建。
    state.watch.clients.lock().unwrap().clear();
    // 凭据/收件人变更已停掉旧入站监听（见 invalidate_changed_routers）；这里立即按新配置重建，
    // 使 `/here`、`/status`、普通消息切槽无需等在途请求结束或 daemon 重启即恢复（有工作中 agent 时）。
    ensure_inbound_listeners(state).await;
    let general = serde_json::to_value(&new.general).unwrap_or(serde_json::Value::Null);
    state
        .registry
        .broadcast_to_guis(ServerMsg::ConfigChanged { general });
    // 配置变更可能刚开启菜单栏图标 → 兜底拉起宿主（宿主自身也监听配置，二者均幂等）。
    maybe_spawn_gui_host(&new);
    // 方案6：弹窗预热开关随配置热切换——开则补热（self-gated），关则回收现有热实例。
    // window_effect 变更时也必须回收：热进程建窗材质在 spawn 时固化，
    // 待命期切到 Blur 若仍领用旧进程且上屏只挂玻璃，会半透明无材质。重建后按新效果 apply_surface。
    let effect_changed = old.general.window_effect != new.general.window_effect;
    if warm_enabled(state) {
        if effect_changed {
            recycle_warm(state);
        }
        maybe_topup_warm(state);
    } else {
        recycle_warm(state);
    }
    // 保活开关热切换：同步 daemon 登录项（保活→写文件、否则→删文件）。关掉保活后不强杀本进程——
    // 空闲循环会按原 5min 策略让其自然退出。
    sync_daemon_login_item();
    log("config reloaded");
}

/// 让 daemon 登录项（开机自启）与 `general.daemonLifecycle` 一致。best-effort、幂等、纯文件操作。
fn sync_daemon_login_item() {
    let keep_alive = crate::config::AppConfig::load_without_secrets()
        .general
        .daemon_lifecycle
        == crate::config::DaemonLifecycleMode::KeepAlive;
    let _ = crate::integrations::login_item::sync_daemon(keep_alive);
}

/// 比对新旧配置：凭据变更或渠道被禁用 → 丢弃对应缓存 Router（惰性失效，Q1）+ 停掉旧入站监听。
///
/// 进行中的请求仍持有自己的 Router `Arc` 克隆，故其连接保留到该请求结束；
/// 下一个请求会经 `ensure_*_router` 用新配置重连。注意：若仅改了同 client_id 的 secret，
/// 且旧请求未结束时新请求又到达，可能短暂出现两条同 client_id 连接（平台会踢掉旧的）——
/// 属配置在「问题进行中」被改动的少见边角，可接受。
///
/// 入站监听单独处理：除凭据变更（连带 Router）外，**收件人 id 变更**（feishu open_id /
/// dingding·slack user_id / telegram chat_id，即监听的 expected_sender）也需停旧监听——否则监听
/// 仍用旧过滤条件绑在旧连接上，`on_config_changed` 随后会按新配置重建（`stop_listener` 释放认领）。
async fn invalidate_changed_routers(state: &Arc<ServerState>, old: &AppConfig, new: &AppConfig) {
    let dd_router_changed = !crate::app::is_dingding_active(new)
        || old.channels.dingding.client_id != new.channels.dingding.client_id
        || old.channels.dingding.client_secret != new.channels.dingding.client_secret;
    if dd_router_changed {
        *state.dd_router.lock().await = None;
    }
    if dd_router_changed || old.channels.dingding.user_id != new.channels.dingding.user_id {
        stop_listener(state, "dingding");
    }

    let fs_router_changed = !crate::app::is_feishu_active(new)
        || old.channels.feishu.app_id != new.channels.feishu.app_id
        || old.channels.feishu.app_secret != new.channels.feishu.app_secret
        || old.channels.feishu.base_url != new.channels.feishu.base_url;
    if fs_router_changed {
        crate::feishu::token::invalidate_credentials(
            &old.channels.feishu.base_url,
            &old.channels.feishu.app_id,
            &old.channels.feishu.app_secret,
        );
        *state.fs_router.lock().await = None;
    }
    if fs_router_changed || old.channels.feishu.open_id != new.channels.feishu.open_id {
        stop_listener(state, "feishu");
    }

    let tg_router_changed = !crate::app::is_telegram_active(new)
        || old.channels.telegram.bot_token != new.channels.telegram.bot_token
        || old.channels.telegram.api_base_url != new.channels.telegram.api_base_url;
    if tg_router_changed {
        *state.tg_router.lock().await = None;
    }
    if tg_router_changed || old.channels.telegram.chat_id != new.channels.telegram.chat_id {
        stop_listener(state, "telegram");
    }

    let sl_router_changed = !crate::app::is_slack_active(new)
        || old.channels.slack.bot_token != new.channels.slack.bot_token
        || old.channels.slack.app_token != new.channels.slack.app_token;
    if sl_router_changed {
        *state.sl_router.lock().await = None;
    }
    if sl_router_changed || old.channels.slack.user_id != new.channels.slack.user_id {
        stop_listener(state, "slack");
    }
}

/// 停掉某渠道的入站监听（改配置时）：take 出其 stop 并 notify——旧任务随即退出、认领立即释放，
/// 供 `ensure_inbound_listeners` 按新配置重建。无监听时为 no-op。
fn stop_listener(state: &Arc<ServerState>, id: &str) {
    if let Some(stop) = state.inbound_listeners.take(id) {
        stop.notify_one();
    }
}

// —— start / stop / restart / status / logs：作为客户端操作 Daemon ——

fn start_cmd() -> i32 {
    block_on(async {
        match client::ensure_running().await {
            Ok(()) => {
                match client::request_status().await {
                    Some(info) => print_status(&info),
                    None => println!("askhuman daemon: running"),
                }
                0
            }
            Err(e) => {
                eprintln!("failed to start daemon: {}", e);
                1
            }
        }
    })
}

fn stop_cmd(force: bool) -> i32 {
    block_on(async {
        if !client::request_stop(force).await {
            println!("askhuman daemon: not running");
            return 0;
        }
        println!("askhuman daemon: stopping");
        wait_stopped(force).await;
        println!("askhuman daemon: stopped");
        0
    })
}

fn restart_cmd(force: bool) -> i32 {
    block_on(async {
        if client::request_stop(force).await {
            wait_stopped(force).await;
        }
        match client::ensure_running().await {
            Ok(()) => {
                println!("askhuman daemon: restarted");
                0
            }
            Err(e) => {
                eprintln!("failed to restart daemon: {}", e);
                1
            }
        }
    })
}

/// 等 Daemon 下线。force：限时即可；graceful：可能在排空（等在途请求完结），
/// 无限等待并周期性打印进度与强制提示。
async fn wait_stopped(force: bool) {
    if force {
        client::wait_until_down(Duration::from_secs(5)).await;
        return;
    }
    let mut last_hint: Option<Instant> = None;
    loop {
        let Some(info) = client::request_status().await else {
            return; // 已下线。
        };
        if info.draining && last_hint.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) {
            eprintln!(
                "askhuman daemon: draining ({} active request(s) left); waiting… (use --force to terminate now)",
                info.active_requests
            );
            last_hint = Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn status_cmd() -> i32 {
    block_on(async {
        match client::request_status().await {
            Some(info) => {
                print_status(&info);
                0
            }
            None => {
                println!("askhuman daemon: not running");
                1
            }
        }
    })
}

fn logs_cmd() -> i32 {
    let p = lifecycle::log_path();
    println!("daemon log: {}", p.display());
    println!("(tip: tail -f {})", p.display());
    0
}

fn print_status(info: &StatusInfo) {
    println!("askhuman daemon: running");
    println!("  pid        {}", info.pid);
    println!(
        "  version    {} (protocol {})",
        info.version, info.protocol_version
    );
    println!("  uptime     {}s", info.uptime_secs);
    println!("  socket     {}", info.socket);
    println!(
        "  requests   {} active{}",
        info.active_requests,
        if info.draining { " (draining)" } else { "" }
    );
    let im = if info.im_connections.is_empty() {
        "none".to_string()
    } else {
        info.im_connections.join(", ")
    };
    println!("  im conns   {}", im);
}

#[cfg(test)]
mod tests {
    use super::{
        agent_kind_for_im, pending_launch_matches, popup_prewarm_requested, popup_should_dispatch,
        select_im_delivery_candidates, task_workspace_options, InboundRegistry, PendingLaunchWatch,
    };
    use crate::agents::AgentKind;
    use crate::config::AppConfig;
    use crate::i18n::Lang;
    use std::sync::Arc;

    #[test]
    fn popup_delivery_requires_channel_and_display() {
        let mut config = AppConfig::default();
        assert!(popup_should_dispatch(&config, true));
        assert!(!popup_should_dispatch(&config, false));

        config.channels.popup.enabled = false;
        assert!(!popup_should_dispatch(&config, true));
        assert!(!popup_should_dispatch(&config, false));
    }

    #[test]
    fn popup_prewarm_requires_popup_channel() {
        let mut config = AppConfig::default();
        assert!(popup_prewarm_requested(&config));

        config.channels.popup.enabled = false;
        assert!(!popup_prewarm_requested(&config));

        config.channels.popup.enabled = true;
        config.general.popup_prewarm = false;
        assert!(!popup_prewarm_requested(&config));
    }

    #[test]
    fn im_delivery_falls_back_only_when_popup_cannot_receive() {
        let available = ["dingding", "feishu", "telegram", "slack"];
        let none = Vec::<String>::new();

        assert_eq!(
            select_im_delivery_candidates(false, &available, Some("popup"), &none, true),
            available
        );
        assert!(
            select_im_delivery_candidates(true, &available, Some("popup"), &none, true).is_empty()
        );
        assert_eq!(
            select_im_delivery_candidates(true, &available, Some("popup"), &none, false),
            available
        );
        assert_eq!(
            select_im_delivery_candidates(true, &available, None, &none, false),
            available
        );
        assert_eq!(
            select_im_delivery_candidates(true, &available, Some("disabled-channel"), &none, false,),
            available
        );
    }

    #[test]
    fn im_delivery_prefers_valid_active_and_watch_candidates() {
        let available = ["dingding", "feishu", "telegram", "slack"];
        assert_eq!(
            select_im_delivery_candidates(true, &available, Some("feishu"), &[], false),
            vec!["feishu"]
        );
        assert_eq!(
            select_im_delivery_candidates(
                true,
                &available,
                Some("popup"),
                &["slack".to_string()],
                false,
            ),
            vec!["slack"]
        );
        assert_eq!(
            select_im_delivery_candidates(
                true,
                &available,
                Some("feishu"),
                &["slack".to_string()],
                false,
            ),
            vec!["feishu", "slack"]
        );
    }

    #[tokio::test]
    async fn im_agent_resolution_uses_known_kind_without_waiting() {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        assert_eq!(
            agent_kind_for_im(Some(AgentKind::Claude), true, Some(rx)).await,
            Some(AgentKind::Claude)
        );
    }

    #[tokio::test]
    async fn im_agent_resolution_accepts_async_mcp_result() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(Some(AgentKind::Codex)).unwrap();
        assert_eq!(
            agent_kind_for_im(None, true, Some(rx)).await,
            Some(AgentKind::Codex)
        );
    }

    #[tokio::test]
    async fn non_mcp_request_does_not_wait_for_agent_resolution() {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        assert_eq!(agent_kind_for_im(None, false, Some(rx)).await, None);
    }

    #[test]
    fn inbound_claim_is_exclusive() {
        let reg = InboundRegistry::default();
        let a = reg.claim("feishu").expect("first claim succeeds");
        assert!(
            reg.claim("feishu").is_none(),
            "a second claim is blocked while the first is held"
        );
        reg.release("feishu", &a);
        assert!(
            reg.claim("feishu").is_some(),
            "the channel can be re-claimed once the owner releases it"
        );
    }

    #[test]
    fn inbound_release_is_identity_safe() {
        // 配置变更场景：take 出旧 stop 释放认领 → 新监听重新认领 → 旧任务**之后**才退出并尝试
        // 释放。释放必须按身份判定，绝不能把「新监听」的认领误删（否则会重复 spawn）。
        let reg = InboundRegistry::default();
        let old = reg.claim("feishu").expect("old listener claims");
        let taken = reg
            .take("feishu")
            .expect("config change takes current stop");
        assert!(Arc::ptr_eq(&old, &taken));
        let new = reg
            .claim("feishu")
            .expect("new listener re-claims after take");
        reg.release("feishu", &old); // 旧任务迟到的释放
        assert!(
            reg.claim("feishu").is_none(),
            "the new listener's claim must survive a stale release from the old task"
        );
        reg.release("feishu", &new);
        assert!(reg.claim("feishu").is_some());
    }

    #[test]
    fn pending_launch_matches_id_or_prompt_hash_but_requires_kind_and_cwd() {
        let item = PendingLaunchWatch {
            id: "launch-1".into(),
            channel: "feishu".into(),
            kind: AgentKind::Codex,
            cwd: "/tmp/project".into(),
            task_sha256: "hash-1".into(),
            created_at: 1,
        };
        assert!(pending_launch_matches(
            &item,
            AgentKind::Codex,
            Some("/tmp/project"),
            Some("launch-1"),
            None
        ));
        assert!(pending_launch_matches(
            &item,
            AgentKind::Codex,
            Some("/tmp/project"),
            None,
            Some("hash-1")
        ));
        assert!(!pending_launch_matches(
            &item,
            AgentKind::Claude,
            Some("/tmp/project"),
            Some("launch-1"),
            None
        ));
        assert!(!pending_launch_matches(
            &item,
            AgentKind::Codex,
            Some("/tmp/other"),
            Some("launch-1"),
            None
        ));
    }

    #[test]
    fn workspace_picker_starts_with_five_and_show_more() {
        let workspaces = (0..7)
            .map(|index| crate::agents::workspaces::Workspace {
                path: format!("/tmp/project-{index}"),
                label: format!("project-{index}"),
                last_used_at: 7 - index,
                agents: vec![],
                pinned: false,
                hidden: false,
            })
            .collect::<Vec<_>>();
        let compact = task_workspace_options(workspaces.clone(), true, Lang::Zh);
        assert_eq!(compact.len(), 6);
        assert_eq!(compact.last().unwrap().id, crate::select::MORE_OPTION_ID);
        let expanded = task_workspace_options(workspaces, false, Lang::Zh);
        assert_eq!(expanded.len(), 7);
        assert!(!expanded
            .iter()
            .any(|option| option.id == crate::select::MORE_OPTION_ID));
    }
}
