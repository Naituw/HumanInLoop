//! CLI 作为 Daemon 客户端：连接 / 握手 / 按需自启 / 换新重连，以及 status·stop 辅助。
//!
//! Phase 0：仅提供连通性原语（ensure_running / request_status / request_stop）。
//! 任务提交（submit）将在 Phase 1 接入。

pub mod composer;

use crate::daemon::lifecycle;
use crate::daemon::spawn;
use crate::ipc::{
    self, transport, ClientHello, ClientMsg, DetectRequest, HelloStatus, ServerMsg, StatusInfo,
};
use std::io::{Error, ErrorKind};
use std::time::{Duration, Instant};
use tokio::io::BufReader;
use transport::{OwnedReadHalf, OwnedWriteHalf};

type Reader = BufReader<OwnedReadHalf>;

/// 本进程的握手信息。
fn hello() -> ClientHello {
    ClientHello {
        protocol_version: ipc::PROTOCOL_VERSION,
        client_version: env!("CARGO_PKG_VERSION").to_string(),
        binary_path: std::env::current_exe()
            .ok()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        fingerprint: lifecycle::current_fingerprint(),
        pid: std::process::id(),
    }
}

/// 连接并拆分读写半。
async fn connect_split() -> std::io::Result<(Reader, OwnedWriteHalf)> {
    let stream = transport::connect().await?;
    let (r, w) = stream.into_split();
    Ok((BufReader::new(r), w))
}

/// 连一次并握手，返回握手状态（连不上返回 None）。
async fn hello_status() -> Option<HelloStatus> {
    let (mut reader, mut writer) = connect_split().await.ok()?;
    ipc::write_msg(&mut writer, &ClientMsg::Hello(hello()))
        .await
        .ok()?;
    match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
        Ok(Some(ServerMsg::HelloAck(ack))) => Some(ack.status),
        _ => None,
    }
}

/// 确保 Daemon 在运行且为当前二进制版本；必要时自启 / 等旧实例换新。
pub async fn ensure_running() -> std::io::Result<()> {
    // 1. 已在运行且 Ok → 直接用。
    match hello_status().await {
        Some(HelloStatus::Ok) => return Ok(()),
        Some(HelloStatus::Restarting) => {
            // 旧实例将自行退出；等它下线后再拉起新的。
            wait_until_down(Duration::from_secs(5)).await;
        }
        // 排空中：快速返回错误（本函数被设置进程 Detect 等复用，不能无限阻塞）；
        // 需要等待的调用方（run_ask / stop / restart）自行处理排空等待。
        Some(HelloStatus::Draining) => {
            return Err(Error::new(ErrorKind::WouldBlock, "daemon is draining"));
        }
        None => {}
    }

    // 2. 拉起并等待就绪（最多约 5 秒）。
    spawn::spawn_detached()?;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if let Some(HelloStatus::Ok) = hello_status().await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err(Error::new(
        ErrorKind::TimedOut,
        "daemon did not become ready in time",
    ))
}

/// 请求运行状态（未运行返回 None）。
pub async fn request_status() -> Option<StatusInfo> {
    let (mut reader, mut writer) = connect_split().await.ok()?;
    ipc::write_msg(&mut writer, &ClientMsg::Status).await.ok()?;
    match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
        Ok(Some(ServerMsg::Status(info))) => Some(info),
        _ => None,
    }
}

/// 权限授权管理面板操作（设置进程 → daemon，spec codex-permission-remember §6.3）。
/// 面板打开时才调用；daemon 未运行则拉起（store 的读写都必须经 daemon）。
pub async fn permission_rules_op(
    op: crate::ipc::PermissionRulesOp,
) -> Result<crate::ipc::PermissionRulesResult, String> {
    ensure_running().await.map_err(|e| e.to_string())?;
    let (mut reader, mut writer) = connect_split().await.map_err(|e| e.to_string())?;
    ipc::write_msg(&mut writer, &ClientMsg::Hello(hello()))
        .await
        .map_err(|e| e.to_string())?;
    loop {
        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(Some(ServerMsg::HelloAck(ack))) => {
                if ack.status == HelloStatus::Ok {
                    break;
                }
                return Err("daemon is restarting".to_string());
            }
            Ok(Some(_)) => continue,
            _ => return Err("daemon connection lost".to_string()),
        }
    }
    ipc::write_msg(&mut writer, &ClientMsg::PermissionRules { op })
        .await
        .map_err(|e| e.to_string())?;
    loop {
        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(Some(ServerMsg::PermissionRules { result })) => return Ok(result),
            Ok(Some(ServerMsg::Error { message })) => return Err(message),
            Ok(Some(_)) => continue,
            _ => return Err("daemon connection lost".to_string()),
        }
    }
}

/// Tell an already-running daemon to reload the persisted update snapshot. This is best-effort and
/// deliberately does not start the daemon merely because Settings or the tray checked for updates.
pub async fn notify_update_state_changed() {
    let Ok((_reader, mut writer)) = connect_split().await else {
        return;
    };
    let _ = ipc::write_msg(&mut writer, &ClientMsg::RefreshUpdateState).await;
}

/// Notify an already-running daemon that an in-app update replaced the on-disk binary.
///
/// First reload the persisted update snapshot so open popups and tray subscribers see `pending`,
/// then send a normal Hello. The Hello makes the daemon compare its startup fingerprint with the
/// new on-disk image immediately: it drains when requests are active and exits at once otherwise.
/// Unlike [`ensure_running`], this deliberately does not start a daemon that is currently stopped.
pub async fn notify_update_applied() {
    notify_update_state_changed().await;
    let _ = hello_status().await;
}

/// GUI 启动新任务后把活跃槽切到 popup（spec gui-agent-task-launch G11）。best-effort、不拉起
/// daemon：daemon 在跑经 IPC 切（含旧渠道反激活回执 / auto-end-watch）；未运行则直接写
/// `auto-channel.json`（daemon 启动时 `load_active` 读回）。
pub async fn activate_popup_slot() {
    match connect_split().await {
        Ok((_reader, mut writer)) => {
            if ipc::write_msg(&mut writer, &ClientMsg::ActivatePopupSlot)
                .await
                .is_err()
            {
                crate::autochannel::save_active(Some("popup"));
            }
        }
        Err(_) => crate::autochannel::save_active(Some("popup")),
    }
}

/// Register a GUI-created launch with an already-running daemon. Fork sources necessarily came
/// from that daemon, so failure simply means lineage correlation will be unavailable.
pub async fn register_launch(record: &crate::integrations::agent_launch::LaunchRecord) {
    let source_session_id = match &record.launch_mode {
        crate::integrations::agent_launch::LaunchMode::New => None,
        crate::integrations::agent_launch::LaunchMode::Fork { source_session_id } => {
            Some(source_session_id.clone())
        }
    };
    if let Ok((_reader, mut writer)) = connect_split().await {
        let _ = ipc::write_msg(
            &mut writer,
            &ClientMsg::RegisterLaunch {
                id: record.id.clone(),
                kind: record.kind,
                cwd: record.cwd.clone(),
                task_sha256: record.task_sha256.clone(),
                source_session_id,
            },
        )
        .await;
    }
}

pub async fn cancel_launch(id: String) {
    if let Ok((_reader, mut writer)) = connect_split().await {
        let _ = ipc::write_msg(&mut writer, &ClientMsg::CancelLaunch { id }).await;
    }
}

/// 请求停止（force=false 为 graceful：有在途请求时 Daemon 排空后退出）；
/// 收到 Stopping 回应返回 true，未运行返回 false。
pub async fn request_stop(force: bool) -> bool {
    let Ok((mut reader, mut writer)) = connect_split().await else {
        return false;
    };
    if ipc::write_msg(&mut writer, &ClientMsg::Stop { force })
        .await
        .is_err()
    {
        return false;
    }
    matches!(
        ipc::read_msg::<_, ServerMsg>(&mut reader).await,
        Ok(Some(ServerMsg::Stopping))
    )
}

/// 轮询直到 Daemon 不可连（或超时）。
pub async fn wait_until_down(max: Duration) {
    let start = Instant::now();
    while start.elapsed() < max {
        if transport::connect().await.is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// 排空等待：旧 Daemon 正在完结在途请求，无限等待其下线（首条提示立即输出，之后每 30s 一条，
/// 含剩余在途数与强制换新提示）。剩余数经 `Status` 查询获取（不带 Hello，不会误触发 stale 判定）。
async fn wait_for_drain() {
    let mut last_hint: Option<Instant> = None;
    loop {
        if transport::connect().await.is_err() {
            return; // 旧 Daemon 已下线，可拉起新的。
        }
        if last_hint.is_none_or(|t| t.elapsed() >= Duration::from_secs(30)) {
            match request_status().await {
                Some(info) => eprintln!(
                    "askhuman: daemon is draining ({} active request(s) left); waiting to submit… (run 'AskHuman daemon restart --force' to switch now, interrupting them)",
                    info.active_requests
                ),
                None => eprintln!(
                    "askhuman: daemon is draining; waiting to submit… (run 'AskHuman daemon restart --force' to switch now)"
                ),
            }
            last_hint = Some(Instant::now());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// 请求 Daemon 执行「自动识别 userId/open_id」（Q6）。
///
/// 返回语义（供设置进程决定是否回退到进程内临时连接）：
/// - `None`：**无法接通 Daemon**（自启失败 / 握手失败）→ 调用方可回退进程内识别。
/// - `Some(Ok(id))`：识别成功。
/// - `Some(Err(msg))`：Daemon 已执行识别但失败（超时 / 断连）→ 调用方**不应**回退（避免再开冲突连接）。
pub async fn request_detect(req: DetectRequest) -> Option<Result<String, String>> {
    if ensure_running().await.is_err() {
        return None;
    }
    let (mut reader, mut writer) = connect_split().await.ok()?;
    if ipc::write_msg(&mut writer, &ClientMsg::Hello(hello()))
        .await
        .is_err()
    {
        return None;
    }
    match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
        Ok(Some(ServerMsg::HelloAck(ack))) if ack.status == HelloStatus::Ok => {}
        // 换新中或握手异常：视作暂不可接通，让调用方回退。
        _ => return None,
    }
    // 握手 OK 后发 Detect；此后的失败都视作「Daemon 已接管」的结果，不再回退。
    if ipc::write_msg(&mut writer, &ClientMsg::Detect(req))
        .await
        .is_err()
    {
        return Some(Err("failed to send detect request".to_string()));
    }
    loop {
        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(Some(ServerMsg::Detected { id })) => return Some(Ok(id)),
            Ok(Some(ServerMsg::Error { message })) => return Some(Err(message)),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return Some(Err("daemon connection lost".to_string())),
        }
    }
}

/// 生命周期事件上报（reporter 用，spec D20）：确保 daemon 在跑（拿不到也尽力直连），
/// 发一条 `AgentEvent` 即走。全程 best-effort，任何失败静默——hook 不能因追踪而拖慢/报错。
pub fn report_agent_event(msg: ClientMsg) {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return;
    };
    rt.block_on(async {
        // 即便 daemon 在排空（WouldBlock）也继续尝试直连：control_loop 不依赖 Hello 即可处理 AgentEvent，
        // 排空中的 daemon 退出前会持久化、由新 daemon 重载。
        let _ = ensure_running().await;
        if let Ok((_, mut writer)) = connect_split().await {
            let _ = ipc::write_msg(&mut writer, &msg).await;
        }
    });
}

/// Best-effort Grok side-channel pending report from the short-lived recovery hook.
pub fn report_grok_binding_pending(msg: ClientMsg) {
    report_agent_event(msg);
}

/// Register the long-lived MCP process before exposing its tools to the client.
pub async fn register_mcp_instance(
    mcp_instance_id: String,
    project: String,
    server_pid: u32,
    parent_pid_hint: Option<u32>,
) {
    let _ = ensure_running().await;
    if let Ok((_, mut writer)) = connect_split().await {
        let _ = ipc::write_msg(
            &mut writer,
            &ClientMsg::McpInstanceRegister {
                mcp_instance_id,
                project,
                server_pid,
                parent_pid_hint,
            },
        )
        .await;
    }
}

/// Claim a unique Grok hook candidate. A short bounded retry covers hook/handler scheduling.
pub async fn claim_grok_binding(
    mcp_instance_id: String,
    project: String,
    tool_name: String,
    arguments_sha256: String,
    server_pid: u32,
) -> Option<String> {
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let Ok((mut reader, mut writer)) = connect_split().await else {
            continue;
        };
        if ipc::write_msg(
            &mut writer,
            &ClientMsg::GrokBindingClaim {
                mcp_instance_id: mcp_instance_id.clone(),
                project: project.clone(),
                tool_name: tool_name.clone(),
                arguments_sha256: arguments_sha256.clone(),
                server_pid,
            },
        )
        .await
        .is_err()
        {
            continue;
        }
        let response = tokio::time::timeout(
            Duration::from_millis(100),
            ipc::read_msg::<_, ServerMsg>(&mut reader),
        )
        .await;
        if let Ok(Ok(Some(ServerMsg::GrokBindingClaim { agent_session_id }))) = response {
            if agent_session_id.is_some() {
                return agent_session_id;
            }
        }
    }
    None
}

/// 插话轮询产物（hook 侧视角，spec agent-interject D3/D4）。
#[derive(Debug, PartialEq, Eq)]
pub enum InterjectPollOutcome {
    /// 放行（无消息 / composer 取消 / daemon 不可达 / 旧 daemon 无回帧 / 任何失败）。
    Allow,
    /// deny + 用户插话消息（hook 侧按家族输出 deny JSON）。
    Deny {
        text: String,
        attachments: Vec<crate::models::FileAttachment>,
    },
}

/// 首帧读取超时：旧 daemon 不认识 `interject_poll`、不会回帧，超时即放行（fail-open），
/// 插话绝不拖慢正常工具调用（spec agent-interject D4）。
const INTERJECT_FIRST_FRAME_TIMEOUT: Duration = Duration::from_millis(300);

/// 带插话轮询的生命周期上报（PreToolUse 专用，spec agent-interject D4）：与 `report_agent_event`
/// 同一连接语义，但发送后读回裁决帧——首帧 300ms 超时放行；`hold` 则无限期等二帧
/// （composer 打开中，等待受 hook 自身 timeout=86400 兜底）。全程 best-effort，失败＝放行。
pub fn report_agent_event_with_poll(msg: ClientMsg) -> InterjectPollOutcome {
    let Ok(rt) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    else {
        return InterjectPollOutcome::Allow;
    };
    rt.block_on(async {
        let _ = ensure_running().await;
        let Ok((mut reader, mut writer)) = connect_split().await else {
            return InterjectPollOutcome::Allow;
        };
        if ipc::write_msg(&mut writer, &msg).await.is_err() {
            return InterjectPollOutcome::Allow;
        }
        interject_read_frames(&mut reader, INTERJECT_FIRST_FRAME_TIMEOUT).await
    })
}

/// 裁决帧读取逻辑（与传输解耦，内存流可单测）：首帧带超时（none/message/hold），
/// hold 后二帧不限时（message/release）。忽略穿插的其它帧；EOF/解析错误＝放行。
async fn interject_read_frames<R>(reader: &mut R, first_timeout: Duration) -> InterjectPollOutcome
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    use crate::ipc::InterjectAction;
    let first = match tokio::time::timeout(first_timeout, read_interject_decision(reader)).await {
        Ok(v) => v,
        Err(_) => return InterjectPollOutcome::Allow, // 超时：旧 daemon / 慢回帧 → 放行
    };
    match first {
        Some((InterjectAction::Message, text, attachments)) => {
            InterjectPollOutcome::Deny { text, attachments }
        }
        Some((InterjectAction::Hold, _, _)) => match read_interject_decision(reader).await {
            Some((InterjectAction::Message, text, attachments)) => {
                InterjectPollOutcome::Deny { text, attachments }
            }
            _ => InterjectPollOutcome::Allow, // release / EOF（daemon 退出等）→ 放行
        },
        _ => InterjectPollOutcome::Allow, // none / release / EOF / 意外帧
    }
}

/// 读到下一帧 `InterjectDecision`（跳过其它服务端消息）；EOF/错误返回 None。
async fn read_interject_decision<R>(
    reader: &mut R,
) -> Option<(
    crate::ipc::InterjectAction,
    String,
    Vec<crate::models::FileAttachment>,
)>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match ipc::read_msg::<_, ServerMsg>(reader).await {
            Ok(Some(ServerMsg::InterjectDecision {
                action,
                text,
                attachments,
            })) => return Some((action, text, attachments)),
            Ok(Some(_)) => continue,
            Ok(None) | Err(_) => return None,
        }
    }
}

/// 状态窗口手动把某 agent 置空闲（纠正漏 hook 卡「工作中」）：即发即走，best-effort。
pub fn force_agent_idle(session_id: String) {
    report_agent_event(ClientMsg::AgentForceIdle { session_id });
}

/// 查询某 session 的待送达插话全文、条目数与附件（控制台气泡，spec gui-agent-console C3）：
/// 一问一答。daemon 未运行时**不拉起**（无 daemon 即无待送达），连不上 / 超时 → 空状态。
pub async fn interject_peek(
    session_id: String,
) -> (String, usize, Vec<crate::models::FileAttachment>) {
    let query = async {
        let (mut reader, mut writer) = connect_split().await.ok()?;
        ipc::write_msg(&mut writer, &ClientMsg::InterjectQuery { session_id })
            .await
            .ok()?;
        loop {
            match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
                Ok(Some(ServerMsg::InterjectState {
                    text,
                    entries,
                    attachments,
                })) => return Some((text, entries, attachments)),
                Ok(Some(_)) => continue,
                _ => return None,
            }
        }
    };
    tokio::time::timeout(Duration::from_millis(800), query)
        .await
        .ok()
        .flatten()
        .unwrap_or((String::new(), 0, Vec::new()))
}

/// 打开一条到 daemon 的连接（订阅状态窗口用，spec D20）：确保在跑后连接并拆分读写半。
pub async fn open_for_subscribe() -> std::io::Result<(Reader, OwnedWriteHalf)> {
    ensure_running().await?;
    connect_split().await
}

/// 取一次 agent 状态快照（headless `agents monitor` 用）：连接 → 握手 → 订阅 → 读首个
/// `AgentsState` 即返回（不持续监听）。daemon 不可达或异常返回 None。
pub async fn request_agents_snapshot() -> Option<serde_json::Value> {
    ensure_running().await.ok()?;
    agents_snapshot_once().await
}

/// 同上，但 daemon 未运行时**不拉起**（待办窗口项目候选用，spec todo-whats-next D9）：
/// 连不上 / 握手非 Ok / 超时直接返回 None，窗口照样可用。
///
/// Bounded by a short timeout so a wedged daemon or non-Ok HelloAck cannot keep the
/// Todos window spinner spinning forever (subscribe otherwise waits on AgentsState).
pub async fn agents_snapshot_if_running() -> Option<serde_json::Value> {
    tokio::time::timeout(Duration::from_millis(800), agents_snapshot_once())
        .await
        .unwrap_or_default()
}

async fn agents_snapshot_once() -> Option<serde_json::Value> {
    let (mut reader, mut writer) = connect_split().await.ok()?;
    ipc::write_msg(&mut writer, &ClientMsg::Hello(hello()))
        .await
        .ok()?;
    // Wait for HelloAck; any non-Ok status (Draining / Restarting) aborts — do not spin.
    loop {
        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(Some(ServerMsg::HelloAck(ack))) => {
                if ack.status == HelloStatus::Ok {
                    break;
                }
                return None;
            }
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
    ipc::write_msg(&mut writer, &ClientMsg::AgentsSubscribe)
        .await
        .ok()?;
    loop {
        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(Some(ServerMsg::AgentsState { agents })) => return Some(agents),
            Ok(Some(_)) => continue,
            _ => return None,
        }
    }
}

/// 瘦客户端 ask 入口：确保 Daemon 在运行 → 握手 → 提交任务 → 流式取回结果 → 按退出码退出（不返回）。
pub fn run_ask(task: crate::ipc::TaskRequest) -> ! {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => std::process::exit(3),
    };
    std::process::exit(rt.block_on(run_ask_async(task)));
}

/// Run an internal Ask request and capture its rendered JSON instead of printing/exiting.
/// Used by the Stop hook; timeout/disconnect/non-success all fail open as `None`.
pub fn run_ask_capture(task: crate::ipc::TaskRequest, timeout: Duration) -> Option<String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async move {
        let final_result = tokio::time::timeout(timeout, run_ask_final_async(task, false))
            .await
            .ok()?;
        (final_result.exit_code == 0).then_some(final_result.stdout)
    })
}

/// Submit a structured permission confirmation. Every transport/protocol/fallback failure returns
/// `None` so the hook writes no decision and the agent keeps its native approval flow.
pub fn run_confirm(task: crate::ipc::ConfirmTask) -> Option<crate::models::ConfirmResult> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(async move {
        ensure_running().await.ok()?;
        let (mut reader, mut writer) = connect_split().await.ok()?;
        ipc::write_msg(&mut writer, &ClientMsg::Hello(hello()))
            .await
            .ok()?;
        match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
            Ok(Some(ServerMsg::HelloAck(ack))) if ack.status == HelloStatus::Ok => {}
            _ => return None,
        }
        ipc::write_msg(&mut writer, &ClientMsg::SubmitConfirm(Box::new(task)))
            .await
            .ok()?;
        read_confirm_frames(&mut reader).await
    })
}

async fn read_confirm_frames<R>(reader: &mut R) -> Option<crate::models::ConfirmResult>
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    loop {
        match ipc::read_msg::<_, ServerMsg>(reader).await {
            Ok(Some(ServerMsg::ConfirmAccepted { .. })) => {}
            Ok(Some(ServerMsg::ConfirmFinal { result })) => return Some(result),
            Ok(Some(ServerMsg::ConfirmFallback { .. })) => return None,
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

struct AskFinal {
    stdout: String,
    exit_code: i32,
}

fn client_error(verbose: bool, message: &str) -> AskFinal {
    if verbose {
        eprintln!("{message}");
    }
    AskFinal {
        stdout: String::new(),
        exit_code: 3,
    }
}

async fn run_ask_async(task: crate::ipc::TaskRequest) -> i32 {
    let result = run_ask_final_async(task, true).await;
    if !result.stdout.is_empty() {
        println!("{}", result.stdout);
    }
    result.exit_code
}

async fn run_ask_final_async(task: crate::ipc::TaskRequest, verbose: bool) -> AskFinal {
    use crate::ipc::ServerMsg;

    // 外层循环：撞上 Daemon 排空（draining）时无限等待其下线，然后重置重试预算重来。
    // 内层：提交前最多重试若干次，覆盖「自启就绪竞争」与「撞上 Daemon 换新」的瞬时失败。
    // 提交被受理后不再重试（避免重复弹窗）。
    'outer: loop {
        for _ in 0..3 {
            match ensure_running().await {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    wait_for_drain().await;
                    continue 'outer;
                }
                Err(_) => {
                    return client_error(verbose, "askhuman: failed to start daemon");
                }
            }
            let Ok((mut reader, mut writer)) = connect_split().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            if ipc::write_msg(&mut writer, &ClientMsg::Hello(hello()))
                .await
                .is_err()
            {
                continue;
            }
            match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
                Ok(Some(ServerMsg::HelloAck(ack))) => match ack.status {
                    HelloStatus::Ok => {}
                    HelloStatus::Restarting => {
                        wait_until_down(Duration::from_secs(5)).await;
                        continue;
                    }
                    HelloStatus::Draining => {
                        wait_for_drain().await;
                        continue 'outer;
                    }
                },
                _ => continue,
            }
            // 提交任务。
            crate::perf::mark(&task.perf_id, "cli.submit");
            if ipc::write_msg(&mut writer, &ClientMsg::Submit(task.clone()))
                .await
                .is_err()
            {
                continue;
            }
            // 流式取回：Warn → stderr；Final → stdout + 退出码；中途断连 → 退出码 3（P4）。
            loop {
                match ipc::read_msg::<_, ServerMsg>(&mut reader).await {
                    Ok(Some(ServerMsg::Accepted { .. })) => {}
                    // 排空闸门拒绝（只出现在 Accepted 之前）：等旧 Daemon 下线后重来。
                    Ok(Some(ServerMsg::Draining { .. })) => {
                        wait_for_drain().await;
                        continue 'outer;
                    }
                    Ok(Some(ServerMsg::Warn { text })) => {
                        if verbose {
                            eprintln!("{}", text);
                        }
                    }
                    Ok(Some(ServerMsg::Final { stdout, exit_code })) => {
                        return AskFinal { stdout, exit_code };
                    }
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => {
                        return client_error(verbose, "askhuman: daemon connection lost");
                    }
                }
            }
        }
        return client_error(verbose, "askhuman: could not reach daemon");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::InterjectAction;
    use tokio::io::BufReader;

    async fn confirm_frames(frames: Vec<ServerMsg>) -> Option<crate::models::ConfirmResult> {
        let (mut tx, rx) = tokio::io::duplex(4096);
        let writer = tokio::spawn(async move {
            for frame in frames {
                ipc::write_msg(&mut tx, &frame).await.unwrap();
            }
        });
        let mut reader = BufReader::new(rx);
        let result = read_confirm_frames(&mut reader).await;
        writer.await.unwrap();
        result
    }

    #[tokio::test]
    async fn confirm_final_is_returned_without_waiting_for_surface_cleanup() {
        let expected = crate::models::ConfirmResult {
            action_id: "approve_once".into(),
            comment: None,
            source_channel_id: "popup".into(),
        };
        let actual = confirm_frames(vec![
            ServerMsg::ConfirmAccepted {
                request_id: "r1".into(),
            },
            ServerMsg::ConfirmFinal {
                result: expected.clone(),
            },
        ])
        .await;
        assert_eq!(actual, Some(expected));
    }

    #[tokio::test]
    async fn confirm_fallback_and_disconnect_return_no_decision() {
        assert_eq!(
            confirm_frames(vec![ServerMsg::ConfirmFallback {
                reason: crate::models::ConfirmFallbackReason::NoAvailableChannel,
            }])
            .await,
            None
        );
        assert_eq!(confirm_frames(vec![]).await, None);
    }

    /// 假 daemon 帧序列 → 裁决逻辑（三态 + 超时 fail-open，spec agent-interject D4）。
    async fn run_frames(
        frames: Vec<ServerMsg>,
        close_after: bool,
        first_timeout: Duration,
    ) -> InterjectPollOutcome {
        let (mut tx, rx) = tokio::io::duplex(4096);
        let mut reader = BufReader::new(rx);
        let writer = tokio::spawn(async move {
            for f in frames {
                ipc::write_msg(&mut tx, &f).await.unwrap();
            }
            if !close_after {
                // 保持连接打开（模拟 daemon 挂着不回帧）。
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        });
        let out = interject_read_frames(&mut reader, first_timeout).await;
        writer.abort();
        out
    }

    #[tokio::test]
    async fn first_frame_none_allows() {
        let out = run_frames(
            vec![ServerMsg::InterjectDecision {
                action: InterjectAction::None,
                text: String::new(),
                attachments: Vec::new(),
            }],
            true,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(out, InterjectPollOutcome::Allow);
    }

    #[tokio::test]
    async fn first_frame_message_denies() {
        let attachment = crate::models::FileAttachment {
            path: "/tmp/screenshot.png".into(),
            name: "screenshot.png".into(),
            size: 42,
            is_image: true,
        };
        let out = run_frames(
            vec![ServerMsg::InterjectDecision {
                action: InterjectAction::Message,
                text: "改用方案 B".into(),
                attachments: vec![attachment.clone()],
            }],
            true,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(
            out,
            InterjectPollOutcome::Deny {
                text: "改用方案 B".into(),
                attachments: vec![attachment],
            }
        );
    }

    #[tokio::test]
    async fn hold_then_message_denies() {
        let out = run_frames(
            vec![
                ServerMsg::InterjectDecision {
                    action: InterjectAction::Hold,
                    text: String::new(),
                    attachments: Vec::new(),
                },
                ServerMsg::InterjectDecision {
                    action: InterjectAction::Message,
                    text: "停一下".into(),
                    attachments: Vec::new(),
                },
            ],
            true,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(
            out,
            InterjectPollOutcome::Deny {
                text: "停一下".into(),
                attachments: Vec::new(),
            }
        );
    }

    #[tokio::test]
    async fn hold_then_release_allows() {
        let out = run_frames(
            vec![
                ServerMsg::InterjectDecision {
                    action: InterjectAction::Hold,
                    text: String::new(),
                    attachments: Vec::new(),
                },
                ServerMsg::InterjectDecision {
                    action: InterjectAction::Release,
                    text: String::new(),
                    attachments: Vec::new(),
                },
            ],
            true,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(out, InterjectPollOutcome::Allow);
    }

    #[tokio::test]
    async fn hold_then_eof_allows() {
        // daemon 在 Hold 后退出（drain）：EOF → 放行。
        let out = run_frames(
            vec![ServerMsg::InterjectDecision {
                action: InterjectAction::Hold,
                text: String::new(),
                attachments: Vec::new(),
            }],
            true,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(out, InterjectPollOutcome::Allow);
    }

    #[tokio::test]
    async fn no_reply_times_out_to_allow() {
        // 旧 daemon 不认识 interject_poll、不回帧：首帧超时 → 放行（fail-open）。
        let started = std::time::Instant::now();
        let out = run_frames(vec![], false, Duration::from_millis(100)).await;
        assert_eq!(out, InterjectPollOutcome::Allow);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[tokio::test]
    async fn immediate_eof_allows() {
        // 连接被立即关闭（daemon 崩溃）→ 放行。
        let out = run_frames(vec![], true, Duration::from_millis(300)).await;
        assert_eq!(out, InterjectPollOutcome::Allow);
    }

    #[tokio::test]
    async fn interleaved_frames_are_skipped() {
        // 穿插其它服务端帧（如广播）不影响裁决读取。
        let out = run_frames(
            vec![
                ServerMsg::Warn {
                    text: "noise".into(),
                },
                ServerMsg::InterjectDecision {
                    action: InterjectAction::Hold,
                    text: String::new(),
                    attachments: Vec::new(),
                },
                ServerMsg::Warn {
                    text: "noise2".into(),
                },
                ServerMsg::InterjectDecision {
                    action: InterjectAction::Message,
                    text: "msg".into(),
                    attachments: Vec::new(),
                },
            ],
            true,
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(
            out,
            InterjectPollOutcome::Deny {
                text: "msg".into(),
                attachments: Vec::new(),
            }
        );
    }
}
