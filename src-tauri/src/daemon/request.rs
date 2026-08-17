//! 请求登记表：每个活动 `request_id` 一套 Coordinator + Channel 任务 + GUI 关联。
//!
//! Phase 1 仅挂 popup adapter：提交时建 Coordinator（Ipc 退出）、注册弹窗 adapter、分配一次性 token；
//! GUI Helper 连上后凭 token 找到该请求、收 `show`、回 `answer`。IM 渠道在 Phase 2 接入。

use crate::app::confirm_coordinator::{ConfirmCoordinator, ConfirmOutcome};
use crate::app::coordinator::Coordinator;
use crate::app::RenderOutcome;
use crate::channels::popup::GuiHelperPopupChannel;
use crate::i18n::Lang;
use crate::ipc::{ConfirmTask, PendingRequestInfo, ServerMsg, ShowPayload, TaskRequest};
use crate::models::{AskRequest, ConfirmDeliveryState, ConfirmRequest, InteractionRequest};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::Notify;

/// GUI 连接的发送端槽位：Helper 连上后填入，供 popup adapter 下发 `cancel`。
pub type GuiSlot = Arc<Mutex<Option<UnboundedSender<ServerMsg>>>>;

/// 异步解析出的调用方 agent 信息（方案5/b）：daemon 从 `caller_pid` 向上 walk 进程树后填入，
/// 经 `ServerMsg::AgentResolved` 后推弹窗 badge；helper 连接时若已就绪也会随握手补发（覆盖竞态）。
#[derive(Clone, Default)]
pub struct ResolvedAgent {
    pub kind: Option<String>,
    pub pid: Option<u32>,
    pub launch_id: Option<String>,
}

/// 一个活动请求的共享状态。
pub struct RequestEntry {
    pub request_id: String,
    /// 单调递增序号（创建顺序）：托盘「待答」子菜单据此稳定按时间排序，不随 HashMap 迭代乱序。
    pub seq: u64,
    /// 一次性 token：GUI Helper 连回握手出示。
    pub token: String,
    /// 抢答协调器（Ipc 退出，结果经 `final_tx` 回传）。
    pub coordinator: Arc<Coordinator>,
    /// 给 GUI Helper 的题目下发负载。
    pub show: ShowPayload,
    /// 调用方 agent 会话 ID（CLI 从 env 探测；MCP 仅在每次调用取得可信绑定时透传）。
    /// 供 daemon「在途 AskHuman 豁免」按 session_id 刷新——覆盖无 pid 的 agent
    /// （Codex 共享 app-server / Claude 被 scrub），使其等待人类回答期间不被「工作中兜底超时」降级。
    pub agent_session_id: Option<String>,
    /// 重复提问收敛用的会话键与提问指纹（spec duplicate-ask-coalescing D1/D2）：
    /// 同键同指纹的后续调用合流到本请求，不再弹第二张卡。会话键为 None 时不参与收敛。
    pub session_key: Option<String>,
    pub fingerprint: String,
    /// 等待本请求结果的调用方个数（原始 CLI + 各 follower）。归零才允许取消整个请求。
    waiters: AtomicUsize,
    /// 等待者归零时唤醒提交连接去做取消收尾（最后离场的可能是 follower）。
    pub waiters_gone: Arc<Notify>,
    /// 合流上来的调用方的结果发送端；终态产生时逐个投递（spec D3/D4）。
    followers: Mutex<Vec<UnboundedSender<RenderOutcome>>>,
    /// GUI 发送端槽位（adapter 与连接处理器共享）。
    pub gui: GuiSlot,
    /// 调用方 agent 异步解析结果（方案5/b）：daemon walk 完成后填入，helper 连接握手时若已就绪则补发。
    pub resolved_agent: Arc<Mutex<Option<ResolvedAgent>>>,
    /// GUI Helper 是否已连上（用于看门狗判定弹窗是否成功拉起）。
    pub gui_connected: AtomicBool,
    /// GUI content and its hidden native window reached the presentation handshake.
    pub gui_ready: AtomicBool,
    /// CLI 断开 / 请求结束时通知 GUI 连接处理器收尾。
    pub cancel: Arc<Notify>,
    /// 渲染结果发送端（协调器 finish 与看门狗共用；连接处理器从对应 rx 取）。
    pub final_tx: UnboundedSender<RenderOutcome>,
}

impl RequestEntry {
    pub fn request(&self) -> &AskRequest {
        self.show
            .interaction
            .ask()
            .expect("ask entry must carry an ask interaction")
    }

    /// 当前等待者个数。
    pub fn waiter_count(&self) -> usize {
        self.waiters.load(Ordering::SeqCst)
    }

    /// 一个调用方离场（连接断开或已拿到结果）：返回剩余等待者个数。
    /// 归零意味着没人再消费结果，由提交连接走取消收尾（spec D4）——最后离场的可能是
    /// 合流上来的调用方，故这里唤醒提交连接，不在各自的任务里各做各的收尾。
    pub fn release_waiter(&self) -> usize {
        let left = self
            .waiters
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                Some(v.saturating_sub(1))
            })
            .map(|prev| prev.saturating_sub(1))
            .unwrap_or(0);
        if left == 0 {
            self.waiters_gone.notify_waiters();
        }
        left
    }

    /// 把终态结果投递给所有合流上来的调用方（各自写自己的 IPC `final`）。
    pub fn broadcast_outcome(&self, outcome: &RenderOutcome) {
        for tx in self.followers.lock().unwrap().drain(..) {
            let _ = tx.send(outcome.clone());
        }
    }
}

/// A structured confirmation entry. Popup/IM transport fields are added independently from Ask so
/// the existing Ask output/history contract remains untouched while both live in one registry.
pub struct ConfirmEntry {
    pub request_id: String,
    pub seq: u64,
    pub token: String,
    pub coordinator: Arc<ConfirmCoordinator>,
    pub request: Arc<ConfirmRequest>,
    pub show: ShowPayload,
    pub source: String,
    pub lang: String,
    pub project: String,
    pub agent_kind: String,
    pub agent_session_id: String,
    pub caller_pid: u32,
    /// Monotonic authority for the fixed confirmation lifetime.
    pub deadline: tokio::time::Instant,
    pub delivery: Mutex<HashMap<String, ConfirmDeliveryState>>,
    pub gui: GuiSlot,
    pub gui_connected: AtomicBool,
    pub cancel: Arc<Notify>,
    /// Set when the winning remember choice could not be persisted and the decision was
    /// degraded to allow-once (spec codex-permission-remember D25); surfaces append a note.
    pub memory_save_failed: Arc<AtomicBool>,
}

#[derive(Clone)]
pub enum InteractionEntry {
    Ask(Arc<RequestEntry>),
    Confirm(Arc<ConfirmEntry>),
}

impl InteractionEntry {
    pub fn request_id(&self) -> &str {
        match self {
            Self::Ask(entry) => &entry.request_id,
            Self::Confirm(entry) => &entry.request_id,
        }
    }

    pub fn token(&self) -> &str {
        match self {
            Self::Ask(entry) => &entry.token,
            Self::Confirm(entry) => &entry.token,
        }
    }

    pub fn seq(&self) -> u64 {
        match self {
            Self::Ask(entry) => entry.seq,
            Self::Confirm(entry) => entry.seq,
        }
    }

    pub fn show(&self) -> &ShowPayload {
        match self {
            Self::Ask(entry) => &entry.show,
            Self::Confirm(entry) => &entry.show,
        }
    }
}

impl ConfirmEntry {
    pub fn start_delivery(&self, channel_id: impl Into<String>) {
        self.delivery
            .lock()
            .unwrap()
            .entry(channel_id.into())
            .or_insert(ConfirmDeliveryState::Starting);
    }

    /// Mark a surface ready only while it is still Starting. A success arriving after timeout is
    /// deliberately rejected so a late popup/card cannot revive a failed request.
    pub fn mark_ready(&self, channel_id: &str, message_id: String) -> bool {
        let mut delivery = self.delivery.lock().unwrap();
        let Some(state) = delivery.get_mut(channel_id) else {
            return false;
        };
        if !matches!(state, ConfirmDeliveryState::Starting) {
            return false;
        }
        *state = ConfirmDeliveryState::Ready { message_id };
        true
    }

    pub fn is_ready(&self, channel_id: &str) -> bool {
        matches!(
            self.delivery.lock().unwrap().get(channel_id),
            Some(ConfirmDeliveryState::Ready { .. })
        )
    }

    pub fn mark_starting_failed(&self, channel_id: &str, reason: impl Into<String>) -> bool {
        let mut delivery = self.delivery.lock().unwrap();
        let Some(state) = delivery.get_mut(channel_id) else {
            return false;
        };
        if !matches!(state, ConfirmDeliveryState::Starting) {
            return false;
        }
        *state = ConfirmDeliveryState::Failed {
            reason: reason.into(),
        };
        delivery
            .values()
            .all(|state| matches!(state, ConfirmDeliveryState::Failed { .. }))
    }

    /// Mark a candidate failed and return whether no Starting/Ready candidates remain.
    pub fn mark_failed(&self, channel_id: &str, reason: impl Into<String>) -> bool {
        let mut delivery = self.delivery.lock().unwrap();
        let Some(state) = delivery.get_mut(channel_id) else {
            return false;
        };
        if matches!(state, ConfirmDeliveryState::Terminal) {
            return false;
        }
        *state = ConfirmDeliveryState::Failed {
            reason: reason.into(),
        };
        !delivery.is_empty()
            && delivery
                .values()
                .all(|state| matches!(state, ConfirmDeliveryState::Failed { .. }))
    }

    pub fn mark_deliveries_terminal(&self) {
        for state in self.delivery.lock().unwrap().values_mut() {
            *state = ConfirmDeliveryState::Terminal;
        }
    }

    pub fn has_delivery(&self, channel_id: &str) -> bool {
        self.delivery.lock().unwrap().contains_key(channel_id)
    }

    pub fn has_live_delivery(&self, channel_id: &str) -> bool {
        matches!(
            self.delivery.lock().unwrap().get(channel_id),
            Some(ConfirmDeliveryState::Starting | ConfirmDeliveryState::Ready { .. })
        )
    }
}

/// Build a source-channel-only structured confirmation owned by daemon functionality rather than
/// an Agent PermissionRequest. It deliberately bypasses the permission-specific registry checks
/// and never exposes a popup token.
pub fn create_internal_confirm(
    spec: crate::models::ConfirmSpec,
    source_channel: &str,
    lang: &str,
    project: &str,
    agent_kind: &str,
    ttl: std::time::Duration,
) -> Result<(Arc<ConfirmEntry>, UnboundedReceiver<ConfirmOutcome>), String> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let created_at_ms = crate::perf::now_ms() as u64;
    let expires_at_ms = created_at_ms.saturating_add(ttl.as_millis() as u64);
    let request = Arc::new(spec.into_request(request_id.clone(), created_at_ms, expires_at_ms)?);
    let (final_tx, final_rx) = tokio::sync::mpsc::unbounded_channel();
    let coordinator = ConfirmCoordinator::new(request.clone(), final_tx);
    let show = ShowPayload {
        request_id: request_id.clone(),
        interaction: InteractionRequest::Confirm((*request).clone()),
        popup_edit: None,
        source: source_channel.to_string(),
        lang: lang.to_string(),
        project: project.to_string(),
        agent_kind: Some(agent_kind.to_string()),
        agent_session_id: None,
        mcp_instance_id: None,
        agent_pid: None,
        agent_console_session_id: None,
        perf_id: String::new(),
        perf_autodismiss: false,
        created_at_ms,
    };
    let entry = Arc::new(ConfirmEntry {
        request_id,
        seq: 0,
        token: String::new(),
        coordinator,
        request,
        show,
        source: source_channel.to_string(),
        lang: lang.to_string(),
        project: project.to_string(),
        agent_kind: agent_kind.to_string(),
        agent_session_id: String::new(),
        caller_pid: 0,
        deadline: tokio::time::Instant::now() + ttl,
        delivery: Mutex::new(HashMap::new()),
        gui: Arc::new(Mutex::new(None)),
        gui_connected: AtomicBool::new(false),
        cancel: Arc::new(Notify::new()),
        memory_save_failed: Arc::new(AtomicBool::new(false)),
    });
    entry.start_delivery(source_channel);
    Ok((entry, final_rx))
}

/// Build the two-phase commit finalizer for a permission memory task (spec
/// codex-permission-remember §5.6/D25/D26): when the winning choice is a remember action,
/// persist its rules before any surface may render a final state; on failure degrade the
/// decision to `approve_once` and flag the entry so surfaces report the unsaved grant.
fn memory_finalizer(
    memory: Option<&crate::permission_rules::PermissionMemory>,
    session_id: &str,
    save_failed: Arc<AtomicBool>,
) -> Option<crate::app::confirm_coordinator::ConfirmFinalizer> {
    let saves = memory
        .map(|memory| memory.saves.clone())
        .filter(|saves| !saves.is_empty())?;
    let session_id = session_id.to_string();
    Some(Arc::new(move |mut result: crate::models::ConfirmResult| {
        let Some(save) = saves.iter().find(|save| save.action_id == result.action_id) else {
            return result;
        };
        // Native config write first: it is the durable promise of an "always allow"
        // choice, so its failure degrades the whole decision (D25).
        if let Some(write) = &save.native {
            if let Err(error) = crate::permission_rules::apply_native_write(write) {
                eprintln!(
                    "[askhuman-daemon] native permission write failed ({error}); degrading {} to approve_once",
                    save.action_id
                );
                save_failed.store(true, Ordering::SeqCst);
                result.action_id = "approve_once".to_string();
                return result;
            }
            eprintln!(
                "[askhuman-daemon] native permission write applied for {}",
                save.action_id
            );
        }
        if save.rules.is_empty() {
            return result;
        }
        match crate::permission_rules::save_rules(&session_id, save.namespace, &save.rules) {
            Ok(()) => {
                eprintln!(
                    "[askhuman-daemon] permission memory saved: session={} action={} rules={}",
                    session_id,
                    save.action_id,
                    save.rules.len()
                );
            }
            Err(error) if save.native.is_some() => {
                // The durable native write already succeeded; a failed session bridge only
                // means this conversation may be asked again. Keep the chosen action.
                eprintln!(
                    "[askhuman-daemon] session bridge save failed ({error:?}) after native write for {}",
                    save.action_id
                );
            }
            Err(error) => {
                eprintln!(
                    "[askhuman-daemon] permission memory save failed ({error:?}); degrading {} to approve_once",
                    save.action_id
                );
                save_failed.store(true, Ordering::SeqCst);
                result.action_id = "approve_once".to_string();
            }
        }
        result
    }))
}

#[derive(Default)]
struct Inner {
    by_id: HashMap<String, Arc<RequestEntry>>,
    confirm_by_id: HashMap<String, Arc<ConfirmEntry>>,
    by_token: HashMap<String, String>,
}

/// 全局请求登记表（Daemon 内唯一）。
pub struct RequestRegistry {
    inner: Mutex<Inner>,
    /// 下一个请求序号（创建顺序，单调递增）。
    next_seq: AtomicU64,
}

impl RequestRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner::default()),
            next_seq: AtomicU64::new(0),
        })
    }

    /// 建立一个请求：分配 request_id / token，建 Coordinator（Ipc）+ popup adapter。
    /// 返回登记项与「渲染结果接收端」（连接处理器据此写 IPC `final`）。
    pub fn create(
        &self,
        task: TaskRequest,
        agent_console_session_id: Option<String>,
    ) -> (Arc<RequestEntry>, UnboundedReceiver<RenderOutcome>) {
        let request_id = uuid::Uuid::new_v4().to_string();
        let token = uuid::Uuid::new_v4().to_string();
        let lang = Lang::resolve(&task.lang);
        // 在 task 被逐字段 move 前取出会话 ID（供在途豁免按 session_id 刷新）。
        let agent_session_id = task
            .agent_session_id
            .clone()
            .filter(|s| !s.trim().is_empty());
        // 收敛键一并算好存下，后续合流查找不必重算（spec duplicate-ask-coalescing D1/D2）。
        let session_key = super::ask_dedup::session_key(&task);
        let fingerprint = super::ask_dedup::fingerprint(&task);

        // Daemon 分配权威 request_id（用于临时目录）。
        let mut request = AskRequest::new(task.message, task.questions, task.is_markdown);
        request.id = request_id.clone();
        request.select_only = task.select_only;
        request.single = task.single;
        request.output_format = task.output_format;
        request.whats_next = task.whats_next;

        let (final_tx, final_rx) = tokio::sync::mpsc::unbounded_channel();
        let coordinator = Coordinator::new_ipc(
            request.clone(),
            lang,
            final_tx.clone(),
            task.project.clone(),
            task.source.clone(),
            crate::app::coordinator::HistoryBinding {
                agent_kind: task.agent_kind.clone(),
                agent_session_id: task.agent_session_id.clone(),
                mcp_instance_id: task.mcp_instance_id.clone(),
            },
            task.record_history,
        );

        let gui: GuiSlot = Arc::new(Mutex::new(None));
        coordinator.register(Arc::new(GuiHelperPopupChannel::new(
            request_id.clone(),
            gui.clone(),
        )));

        let show_agent_session_id = task.agent_session_id.clone();
        let show_mcp_instance_id = task.mcp_instance_id.clone();
        let show = ShowPayload {
            request_id: request_id.clone(),
            interaction: InteractionRequest::Ask(request),
            popup_edit: None,
            source: task.source,
            lang: task.lang,
            project: task.project,
            agent_kind: task.agent_kind,
            agent_session_id: show_agent_session_id,
            mcp_instance_id: show_mcp_instance_id,
            agent_pid: task.agent_pid,
            agent_console_session_id,
            // 方案6：透传 perf 上下文，热 helper 领用时据此开启埋点（无 env 也能量化热路径）。
            perf_id: task.perf_id,
            perf_autodismiss: task.perf_autodismiss,
            // 提问创建时刻：弹窗相对时间的锚点（预热弹窗领用时即为真正到达时刻）。
            created_at_ms: crate::perf::now_ms() as u64,
        };

        let entry = Arc::new(RequestEntry {
            request_id: request_id.clone(),
            seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
            token: token.clone(),
            coordinator,
            show,
            agent_session_id,
            session_key,
            fingerprint,
            waiters: AtomicUsize::new(1),
            waiters_gone: Arc::new(Notify::new()),
            followers: Mutex::new(Vec::new()),
            gui,
            resolved_agent: Arc::new(Mutex::new(None)),
            gui_connected: AtomicBool::new(false),
            gui_ready: AtomicBool::new(false),
            cancel: Arc::new(Notify::new()),
            final_tx,
        });

        let mut inner = self.inner.lock().unwrap();
        inner.by_id.insert(request_id, entry.clone());
        inner.by_token.insert(token, entry.request_id.clone());
        (entry, final_rx)
    }

    /// Build a validated structured confirmation with daemon-owned identity and 24h deadline.
    /// 合流查找（spec duplicate-ask-coalescing D3/D9）：同会话键 + 同指纹且**仍在等人回答**的
    /// 在途请求存在时，把调用方登记为 follower，返回其结果接收端。
    ///
    /// 查找、收尾判定与登记必须在同一把锁内完成：已进入收尾（终态已产生）的请求视为不在途，
    /// 否则 follower 会挂在一个永远不再投递的请求上。在途请求是个位数，直接遍历即可，不建索引。
    pub fn try_attach(
        &self,
        session_key: &str,
        fingerprint: &str,
    ) -> Option<(Arc<RequestEntry>, UnboundedReceiver<RenderOutcome>)> {
        let inner = self.inner.lock().unwrap();
        let entry = inner.by_id.values().find(|entry| {
            entry.session_key.as_deref() == Some(session_key)
                && entry.fingerprint == fingerprint
                && !entry.coordinator.is_finalizing()
        })?;
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        entry.followers.lock().unwrap().push(tx);
        entry.waiters.fetch_add(1, Ordering::SeqCst);
        Some((entry.clone(), rx))
    }

    pub fn create_confirm(
        &self,
        task: ConfirmTask,
        agent_console_session_id: Option<String>,
    ) -> Result<(Arc<ConfirmEntry>, UnboundedReceiver<ConfirmOutcome>), String> {
        const REQUIRED_CONTEXT: [&str; 6] = [
            "agent",
            "project",
            "workspace",
            "tool",
            "permission_mode",
            "created_at",
        ];
        for required in REQUIRED_CONTEXT {
            if !task.spec.context.iter().any(|field| field.id == required) {
                return Err(format!(
                    "permission confirmation missing context: {required}"
                ));
            }
        }
        if !matches!(task.agent_kind.as_str(), "claude" | "codex") {
            return Err("confirm agent must be claude or codex".to_string());
        }
        if task.agent_session_id.trim().is_empty() {
            return Err("confirm requires an agent session id".to_string());
        }
        if let Some(intent) = task.popup_edit.as_ref() {
            crate::permission_diff::validate_intent(intent, &task.agent_kind, &task.project)?;
        }
        if let Some(memory) = task.memory.as_ref() {
            if task.agent_kind != "codex" {
                return Err("permission memory is supported for codex only".to_string());
            }
            let choice_ids: Vec<&str> = task
                .spec
                .choices
                .iter()
                .map(|choice| choice.id.as_str())
                .collect();
            memory.validate(&choice_ids)?;
        }
        let request_id = uuid::Uuid::new_v4().to_string();
        let token = uuid::Uuid::new_v4().to_string();
        let created_at_ms = crate::perf::now_ms() as u64;
        const TTL_SECS: u64 = 24 * 60 * 60;
        let expires_at_ms = created_at_ms.saturating_add(TTL_SECS * 1000);
        let request = Arc::new(task.spec.into_request(
            request_id.clone(),
            created_at_ms,
            expires_at_ms,
        )?);
        let (final_tx, final_rx) = tokio::sync::mpsc::unbounded_channel();
        let memory_save_failed = Arc::new(AtomicBool::new(false));
        let finalizer = memory_finalizer(
            task.memory.as_ref(),
            &task.agent_session_id,
            memory_save_failed.clone(),
        );
        let coordinator = ConfirmCoordinator::with_finalizer(request.clone(), final_tx, finalizer);
        let show = ShowPayload {
            request_id: request_id.clone(),
            interaction: InteractionRequest::Confirm((*request).clone()),
            popup_edit: task.popup_edit.clone(),
            source: task.source.clone(),
            lang: task.lang.clone(),
            project: task.project.clone(),
            agent_kind: Some(task.agent_kind.clone()),
            agent_session_id: Some(task.agent_session_id.clone()),
            mcp_instance_id: None,
            agent_pid: None,
            agent_console_session_id,
            perf_id: String::new(),
            perf_autodismiss: false,
            created_at_ms,
        };
        let entry = Arc::new(ConfirmEntry {
            request_id: request_id.clone(),
            seq: self.next_seq.fetch_add(1, Ordering::SeqCst),
            token: token.clone(),
            coordinator,
            request,
            show,
            source: task.source,
            lang: task.lang,
            project: task.project,
            agent_kind: task.agent_kind,
            agent_session_id: task.agent_session_id,
            caller_pid: task.caller_pid,
            deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(TTL_SECS),
            delivery: Mutex::new(HashMap::new()),
            gui: Arc::new(Mutex::new(None)),
            gui_connected: AtomicBool::new(false),
            cancel: Arc::new(Notify::new()),
            memory_save_failed,
        });
        let mut inner = self.inner.lock().unwrap();
        inner.confirm_by_id.insert(request_id, entry.clone());
        inner.by_token.insert(token, entry.request_id.clone());
        Ok((entry, final_rx))
    }

    /// GUI Helper 凭 token 关联请求（token 一次性，关联后即注销）。
    pub fn attach_gui(&self, token: &str) -> Option<InteractionEntry> {
        let mut inner = self.inner.lock().unwrap();
        let request_id = inner.by_token.remove(token)?;
        inner
            .by_id
            .get(&request_id)
            .cloned()
            .map(InteractionEntry::Ask)
            .or_else(|| {
                inner
                    .confirm_by_id
                    .get(&request_id)
                    .cloned()
                    .map(InteractionEntry::Confirm)
            })
    }

    /// 移除请求（结束 / 取消）。
    pub fn remove(&self, request_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.by_id.remove(request_id) {
            inner.by_token.remove(&entry.token);
        }
    }

    pub fn remove_confirm(&self, request_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.confirm_by_id.remove(request_id) {
            inner.by_token.remove(&entry.token);
        }
    }

    /// 活动请求数（供 status）。
    pub fn active_count(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        inner.by_id.len() + inner.confirm_by_id.len()
    }

    /// 当前所有在途请求项的快照（供「补推在途」把已发问题补发到新激活的渠道）。
    pub fn in_flight_entries(&self) -> Vec<Arc<RequestEntry>> {
        self.inner.lock().unwrap().by_id.values().cloned().collect()
    }

    pub fn in_flight_confirm_entries(&self) -> Vec<Arc<ConfirmEntry>> {
        self.inner
            .lock()
            .unwrap()
            .confirm_by_id
            .values()
            .cloned()
            .collect()
    }

    /// 所有在途请求关联的调用方 agent pid（去重）。供 daemon「工作中兜底超时」豁免——
    /// 凡有在途 AskHuman 请求的 agent（正等待人类回答）都持续刷新活动、不被降级为空闲。
    /// 优先用异步解析出的 `resolved_agent.pid`（最准、当次现取），否则回退 `show.agent_pid`。
    pub fn in_flight_agent_pids(&self) -> Vec<u32> {
        let inner = self.inner.lock().unwrap();
        let mut pids: Vec<u32> = Vec::new();
        for entry in inner.by_id.values() {
            let pid = entry
                .resolved_agent
                .lock()
                .ok()
                .and_then(|g| g.as_ref().and_then(|a| a.pid))
                .or(entry.show.agent_pid);
            if let Some(pid) = pid {
                if pid != 0 && !pids.contains(&pid) {
                    pids.push(pid);
                }
            }
        }
        for entry in inner.confirm_by_id.values() {
            if entry.caller_pid != 0 && !pids.contains(&entry.caller_pid) {
                pids.push(entry.caller_pid);
            }
        }
        pids
    }

    /// 所有在途请求关联的调用方 agent 会话 ID（去重）。供 daemon「工作中兜底超时」豁免的
    /// **session_id 版**——覆盖无 pid 的 agent（Codex 共享 app-server / Claude 被 scrub），使其
    /// 等待人类回答期间不被降级为空闲（有 pid 的由 `in_flight_agent_pids` 覆盖）。
    pub fn in_flight_agent_session_ids(&self) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let mut ids: Vec<String> = Vec::new();
        for entry in inner.by_id.values() {
            if let Some(sid) = entry.agent_session_id.as_ref() {
                if !sid.is_empty() && !ids.contains(sid) {
                    ids.push(sid.clone());
                }
            }
        }
        for entry in inner.confirm_by_id.values() {
            if !entry.agent_session_id.is_empty() && !ids.contains(&entry.agent_session_id) {
                ids.push(entry.agent_session_id.clone());
            }
        }
        ids
    }

    /// 在途请求的 `(agent session_id, request_id, 预览)` 映射（同 session 多请求取最早登记的）。
    /// 供状态窗口快照注入 `waitingRequestId`/`waitingPreview`（spec gui-agent-console C7/R2）：
    /// 边栏 🙋 徽标 + 等待横幅摘要 + 「去回答」精确聚焦对应弹窗。
    pub fn in_flight_agent_requests(&self) -> Vec<(String, String, String)> {
        let mut rows: Vec<(u64, (String, String, String))> = {
            let inner = self.inner.lock().unwrap();
            inner
                .by_id
                .values()
                .filter_map(|entry| {
                    entry
                        .agent_session_id
                        .as_ref()
                        .filter(|sid| !sid.is_empty())
                        .map(|sid| {
                            (
                                entry.seq,
                                (
                                    sid.clone(),
                                    entry.request_id.clone(),
                                    preview_of(entry.request()),
                                ),
                            )
                        })
                })
                .chain(
                    inner
                        .confirm_by_id
                        .values()
                        .filter(|entry| !entry.agent_session_id.is_empty())
                        .map(|entry| {
                            (
                                entry.seq,
                                (
                                    entry.agent_session_id.clone(),
                                    entry.request_id.clone(),
                                    truncate_chars(
                                        &entry.request.detail.summary,
                                        PREVIEW_MAX_CHARS,
                                    ),
                                ),
                            )
                        }),
                )
                .collect()
        };
        rows.sort_by_key(|(seq, _)| *seq);
        let mut out: Vec<(String, String, String)> = Vec::new();
        for (_, row) in rows {
            if !out.iter().any(|(s, _, _)| s == &row.0) {
                out.push(row);
            }
        }
        out
    }

    /// 在途请求摘要（按创建顺序，托盘「待答」子菜单用）：每条 `{id, 预览}`。
    pub fn pending_infos(&self) -> Vec<PendingRequestInfo> {
        let mut entries: Vec<(u64, PendingRequestInfo)> = {
            let inner = self.inner.lock().unwrap();
            inner
                .by_id
                .values()
                .map(|entry| {
                    (
                        entry.seq,
                        PendingRequestInfo {
                            id: entry.request_id.clone(),
                            preview: preview_of(entry.request()),
                        },
                    )
                })
                .chain(inner.confirm_by_id.values().map(|entry| {
                    (
                        entry.seq,
                        PendingRequestInfo {
                            id: entry.request_id.clone(),
                            preview: truncate_chars(
                                &entry.request.detail.summary,
                                PREVIEW_MAX_CHARS,
                            ),
                        },
                    )
                }))
                .collect()
        };
        entries.sort_by_key(|(seq, _)| *seq);
        entries.into_iter().map(|(_, info)| info).collect()
    }

    /// Send one message to a request's live popup helper.
    pub fn send_to_gui(&self, request_id: &str, msg: ServerMsg) -> bool {
        let inner = self.inner.lock().unwrap();
        let gui = if let Some(entry) = inner.by_id.get(request_id) {
            &entry.gui
        } else if let Some(entry) = inner.confirm_by_id.get(request_id) {
            &entry.gui
        } else {
            return false;
        };
        let Ok(slot) = gui.lock() else {
            return false;
        };
        match slot.as_ref() {
            Some(tx) => tx.send(msg).is_ok(),
            None => false,
        }
    }

    /// Cancel every active request (daemon shutdown): interrupt all their channels as a generic
    /// `Cancelled` (no source) so IM cards finalize and popups close, and wake their GUI handlers.
    /// Returns the number of requests affected, so the caller can decide whether to wait for the
    /// IM finalize HTTP calls to land before the runtime exits.
    pub fn cancel_all_requests(&self) -> usize {
        let inner = self.inner.lock().unwrap();
        for entry in inner.by_id.values() {
            entry.coordinator.cancel_request(String::new(), "system");
            entry.cancel.notify_waiters();
        }
        for entry in inner.confirm_by_id.values() {
            entry.coordinator.cancel();
            entry.cancel.notify_waiters();
        }
        inner.by_id.len() + inner.confirm_by_id.len()
    }

    /// 向所有已连上的活动 GUI Helper 广播一条消息（如 `ConfigChanged` 实时切主题/语言，A12）。
    pub fn broadcast_to_guis(&self, msg: ServerMsg) {
        let inner = self.inner.lock().unwrap();
        for entry in inner.by_id.values() {
            if let Ok(slot) = entry.gui.lock() {
                if let Some(tx) = slot.as_ref() {
                    let _ = tx.send(msg.clone());
                }
            }
        }
        for entry in inner.confirm_by_id.values() {
            if let Ok(slot) = entry.gui.lock() {
                if let Some(tx) = slot.as_ref() {
                    let _ = tx.send(msg.clone());
                }
            }
        }
    }
}

/// 看门狗等待时长：GUI Helper 在此时长内未连上即判定弹窗拉起失败。
pub const GUI_CONNECT_TIMEOUT_SECS: u64 = 10;

/// 弹窗拉起失败时给 CLI 的退出码（无可用 Channel）。
pub const EXIT_NO_CHANNEL: i32 = crate::app::EXIT_NO_CHANNEL;

/// 构造「弹窗拉起失败」的渲染结果（→ CLI stderr + 退出码 3）。
pub fn popup_failed_outcome(lang: Lang) -> RenderOutcome {
    RenderOutcome {
        stdout: String::new(),
        stderr: Some(format!(
            "{}{}",
            crate::i18n::err_prefix(lang),
            "GUI popup failed to start",
        )),
        exit_code: EXIT_NO_CHANNEL,
    }
}

/// 用于 ServerMsg::Show 的便捷封装。
pub fn show_msg(entry: &RequestEntry) -> ServerMsg {
    ServerMsg::Show(entry.show.clone())
}

/// 托盘「待答」子菜单预览：取 Message 首个非空行，空则取第一题题干，截断到 24 个字符（超出加 …）。
const PREVIEW_MAX_CHARS: usize = 24;

pub fn preview_of(req: &AskRequest) -> String {
    let source = first_nonempty_line(&req.message.text)
        .or_else(|| {
            req.questions
                .first()
                .and_then(|q| first_nonempty_line(&q.message))
        })
        .unwrap_or_default();
    truncate_chars(&source, PREVIEW_MAX_CHARS)
}

fn first_nonempty_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(|l| l.to_string())
}

/// 按 Unicode 字符（而非字节）截断，超出追加省略号。
fn truncate_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::confirm::ActionRole;
    use crate::models::{
        ConfirmChoice, ConfirmDetail, ConfirmField, ConfirmFieldKind, ConfirmPresentation,
        ConfirmSpec,
    };
    use crate::permission_diff::adapters::{normalize_permission_edit, AdapterOutcome};
    use serde_json::json;

    fn confirm_task() -> ConfirmTask {
        let context = [
            "agent",
            "project",
            "workspace",
            "tool",
            "permission_mode",
            "created_at",
        ]
        .into_iter()
        .map(|id| ConfirmField {
            id: id.into(),
            label: id.into(),
            value: "value".into(),
            kind: ConfirmFieldKind::Text,
        })
        .collect();
        ConfirmTask {
            spec: ConfirmSpec {
                title: "Permission request".into(),
                context,
                detail: ConfirmDetail {
                    summary: "Run command".into(),
                    body_md: String::new(),
                },
                choices: vec![
                    ConfirmChoice {
                        id: "approve_once".into(),
                        label: "Approve once".into(),
                        description: String::new(),
                        role: ActionRole::Primary,
                        variant: None,
                    },
                    ConfirmChoice {
                        id: "deny".into(),
                        label: "Deny".into(),
                        description: String::new(),
                        role: ActionRole::Destructive,
                        variant: None,
                    },
                ],
                presentation: ConfirmPresentation::SingleSelectSubmit {
                    input: None,
                    submit_label: "Submit".into(),
                    default_action_id: None,
                },
                dismiss_action_id: "deny".into(),
            },
            popup_edit: None,
            source: "Claude Code".into(),
            lang: "en".into(),
            project: "/tmp/project".into(),
            agent_kind: "claude".into(),
            agent_session_id: "session-1".into(),
            caller_pid: 42,
            memory: None,
        }
    }

    #[test]
    fn ask_registry_propagates_exact_recovery_binding_into_coordinator() {
        let task: TaskRequest = serde_json::from_value(json!({
            "message": {"text": "context", "files": []},
            "questions": [{"message": "continue?", "predefinedOptions": []}],
            "isMarkdown": true,
            "source": "Cursor",
            "lang": "en",
            "project": "/tmp/project",
            "agentKind": "cursor",
            "agentSessionId": "conversation-1",
            "mcpInstanceId": "instance-1",
            "fromMcp": true
        }))
        .unwrap();
        let registry = RequestRegistry::new();
        let (entry, _rx) = registry.create(task, Some("conversation-1".into()));
        assert_eq!(
            entry.coordinator.recovery_binding(),
            (
                Some("cursor".into()),
                Some("conversation-1".into()),
                Some("instance-1".into())
            )
        );
        assert_eq!(entry.agent_session_id.as_deref(), Some("conversation-1"));
        assert_eq!(
            entry.show.agent_session_id.as_deref(),
            Some("conversation-1")
        );
        assert_eq!(entry.show.mcp_instance_id.as_deref(), Some("instance-1"));
        assert_eq!(
            entry.show.agent_console_session_id.as_deref(),
            Some("conversation-1")
        );
    }

    fn ask_task(session: Option<&str>, question: &str) -> TaskRequest {
        serde_json::from_value(json!({
            "message": {"text": "context", "files": []},
            "questions": [{"message": question, "predefinedOptions": []}],
            "isMarkdown": true,
            "source": "Cursor",
            "lang": "en",
            "project": "/tmp/project",
            "agentKind": "cursor",
            "agentSessionId": session,
        }))
        .unwrap()
    }

    #[test]
    fn identical_ask_from_same_session_attaches_to_the_live_request() {
        let registry = RequestRegistry::new();
        let (entry, _rx) = registry.create(ask_task(Some("s1"), "continue?"), None);
        let key = entry.session_key.clone().unwrap();
        assert_eq!(key, "sid:s1");
        assert_eq!(entry.waiter_count(), 1);

        // 不同会话 / 不同指纹都不合流。
        assert!(registry
            .try_attach("sid:other", &entry.fingerprint)
            .is_none());
        let different =
            super::super::ask_dedup::fingerprint(&ask_task(Some("s1"), "something else"));
        assert!(registry.try_attach(&key, &different).is_none());

        let (same, mut rx) = registry
            .try_attach(&key, &entry.fingerprint)
            .expect("identical ask coalesces");
        assert_eq!(same.request_id, entry.request_id);
        assert_eq!(entry.waiter_count(), 2);

        let outcome = RenderOutcome {
            stdout: "[user_input]\nyes".into(),
            stderr: None,
            exit_code: 0,
        };
        entry.broadcast_outcome(&outcome);
        assert_eq!(rx.try_recv().unwrap().stdout, "[user_input]\nyes");

        assert_eq!(entry.release_waiter(), 1);
        assert_eq!(entry.release_waiter(), 0);
    }

    #[test]
    fn ask_without_session_never_coalesces() {
        let registry = RequestRegistry::new();
        let (entry, _rx) = registry.create(ask_task(None, "continue?"), None);
        assert!(entry.session_key.is_none());
        assert!(registry.try_attach("sid:s1", &entry.fingerprint).is_none());
    }

    #[tokio::test]
    async fn finalizing_request_is_not_attachable() {
        let registry = RequestRegistry::new();
        let (entry, _rx) = registry.create(ask_task(Some("s1"), "continue?"), None);
        let key = entry.session_key.clone().unwrap();
        entry
            .coordinator
            .submit(crate::models::ChannelResult::cancel("popup"));
        // 终态已产生：再挂上去就永远等不到结果了（spec duplicate-ask-coalescing D9）。
        assert!(registry.try_attach(&key, &entry.fingerprint).is_none());
    }

    #[test]
    fn permission_context_is_required_before_daemon_identity_is_allocated() {
        let registry = RequestRegistry::new();
        let mut task = confirm_task();
        task.spec.context.retain(|field| field.id != "tool");
        let error = registry.create_confirm(task, None).err().unwrap();
        assert!(error.contains("missing context: tool"));
        assert_eq!(registry.active_count(), 0);
    }

    #[test]
    fn confirm_registry_owns_identity_deadline_and_typed_gui_token() {
        let registry = RequestRegistry::new();
        let before = tokio::time::Instant::now();
        let (entry, _rx) = registry
            .create_confirm(confirm_task(), Some("session-1".into()))
            .unwrap();
        assert_eq!(entry.request.id, entry.request_id);
        assert_eq!(
            entry.request.expires_at_ms - entry.request.created_at_ms,
            86_400_000
        );
        assert!(entry.deadline >= before + std::time::Duration::from_secs(86_399));
        assert_eq!(registry.in_flight_agent_pids(), vec![42]);
        assert_eq!(registry.in_flight_agent_session_ids(), vec!["session-1"]);
        assert_eq!(
            entry.show.agent_console_session_id.as_deref(),
            Some("session-1")
        );
        assert!(matches!(
            registry.attach_gui(&entry.token),
            Some(InteractionEntry::Confirm(_))
        ));
    }

    #[test]
    fn popup_edit_is_forwarded_only_in_show_payload() {
        let mut task = confirm_task();
        let AdapterOutcome::Intent(intent) = normalize_permission_edit(
            "claude",
            "Edit",
            &json!({
                "file_path": "/tmp/project/a.txt",
                "old_string": "old",
                "new_string": "new"
            }),
            "/tmp/project",
        ) else {
            panic!("expected intent");
        };
        task.popup_edit = Some(intent);
        let registry = RequestRegistry::new();
        let (entry, _rx) = registry.create_confirm(task, None).unwrap();
        assert!(entry.show.popup_edit.is_some());
        let request_json = serde_json::to_string(&entry.request).unwrap();
        assert!(!request_json.contains("popupEdit"));
        assert!(!request_json.contains("oldText"));
    }

    #[test]
    fn mismatched_popup_edit_is_rejected() {
        let mut task = confirm_task();
        let AdapterOutcome::Intent(mut intent) = normalize_permission_edit(
            "claude",
            "Edit",
            &json!({
                "file_path": "/tmp/project/a.txt",
                "old_string": "old",
                "new_string": "new"
            }),
            "/tmp/project",
        ) else {
            panic!("expected intent");
        };
        intent.agent_kind = "codex".into();
        task.popup_edit = Some(intent);
        let registry = RequestRegistry::new();
        assert!(registry.create_confirm(task, None).is_err());
    }

    #[test]
    fn late_ready_cannot_revive_a_failed_delivery() {
        let registry = RequestRegistry::new();
        let (entry, _rx) = registry.create_confirm(confirm_task(), None).unwrap();
        entry.start_delivery("popup");
        assert!(entry.mark_starting_failed("popup", "timeout"));
        assert!(!entry.mark_ready("popup", String::new()));
        assert!(!entry.is_ready("popup"));
    }

    /// `in_flight_agent_requests`（spec gui-agent-console C7/R2）：session → request_id 映射，
    /// 按登记顺序、同 session 去重取最早，ask 与 confirm 都计入。
    #[test]
    fn in_flight_agent_requests_maps_sessions_in_seq_order() {
        let ask = |sid: &str| -> TaskRequest {
            serde_json::from_value(json!({
                "message": {"text": "q", "files": []},
                "questions": [{"message": "continue?", "predefinedOptions": []}],
                "isMarkdown": true,
                "source": "Cursor",
                "lang": "en",
                "project": "/tmp/project",
                "agentKind": "cursor",
                "agentSessionId": sid,
            }))
            .unwrap()
        };
        let registry = RequestRegistry::new();
        let (a1, _rx1) = registry.create(ask("s-ask"), None);
        let (a2, _rx2) = registry.create(ask("s-ask"), None); // 同 session 第二条：应被去重忽略
        let (c1, _rx3) = registry.create_confirm(confirm_task(), None).unwrap(); // session-1
        let rows = registry.in_flight_agent_requests();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "s-ask");
        assert_eq!(rows[0].1, a1.request_id);
        assert!(!rows[0].2.is_empty(), "ask preview should be non-empty");
        assert_eq!(rows[1].0, "session-1");
        assert_eq!(rows[1].1, c1.request_id);
        assert_ne!(rows[0].1, a2.request_id);
        // 完结后消失。
        registry.remove(&a1.request_id);
        registry.remove(&a2.request_id);
        registry.remove_confirm(&c1.request_id);
        assert!(registry.in_flight_agent_requests().is_empty());
    }
}
