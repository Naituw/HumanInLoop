//! 抢答协调器：并行 Channel 的首个终态结果生效，其余被 `interrupt` 收尾。
//!
//! 收到首个结果后不立即退出，而是给落败渠道一个**收尾窗口**（最多 ~2s，事件驱动、提前结束）
//! 把卡片改成终态（钉钉灰显「已提交」、Telegram 编辑卡片为「已回答」等），随后输出结果并退出。

use super::RenderOutcome;
use crate::channels::{Channel, Interruption, Preemption};
use crate::i18n::{self, Lang};
use crate::models::{AskRequest, ChannelAction, ChannelResult};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tokio::sync::mpsc::UnboundedSender;

use super::terminal_gate::FirstTerminalGate;

/// 收尾窗口上限：超过即强制退出，保证进程不会因某端收尾卡住而挂起。
/// 事件驱动为主（落败端收尾完成即提前退出），此上限仅为兜底；取值偏宽以容忍
/// 跨网络编辑卡片（如代理下访问 Telegram）较慢的情况。
const FINALIZE_TIMEOUT: Duration = Duration::from_secs(5);

/// 拿到结果后如何退出进程。
#[derive(Clone)]
pub enum Exiter {
    /// GUI 模式：经 Tauri 事件循环退出（携带退出码）。
    Gui(AppHandle),
    /// headless 模式：直接退出进程。
    Process,
    /// Daemon 模式：不退出进程，把渲染好的结果经通道回传连接处理器（由它写 IPC `final`）。
    Ipc(UnboundedSender<RenderOutcome>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct HistoryBinding {
    pub agent_kind: Option<String>,
    pub agent_session_id: Option<String>,
    pub mcp_instance_id: Option<String>,
}

pub struct Coordinator {
    inner: Mutex<Inner>,
    terminal: FirstTerminalGate<()>,
    /// 结果渲染 / 收尾文案使用的界面语言（Daemon 模式为调用方上送的 lang；单进程为 `Lang::current()`）。
    lang: Lang,
    /// 当前项目 key（用于回复历史归类；可空）。
    project: String,
    /// 调用方来源名（写入回复历史；可空）。
    source: String,
    /// 调用方 agent 家族（claude/codex/cursor/grok，写入回复历史；可空）。
    /// 内部可变：daemon 异步 walk 进程树解析完成后经 `set_agent_kind` 回填
    /// （MCP 模式 env 判不出家族，只有这条路能拿到），`finish` 落历史时取最新值。
    agent_kind: Mutex<Option<String>>,
    /// Native Agent session/conversation id used for exact history recovery.
    agent_session_id: Option<String>,
    /// Process-scoped MCP instance fallback partition.
    mcp_instance_id: Option<String>,
    /// 仍在收尾的落败「消息渠道」数（弹窗瞬时关闭，不计入）。
    pending: Arc<AtomicUsize>,
    /// 已采纳的终态结果（首个 submit 写入）。
    result: Mutex<Option<ChannelResult>>,
    /// 赢家渠道 id（首个 submit 写入；与 `result` 不同，`finish` 不会取走，供作答后更新活跃槽读取）。
    winner: Mutex<Option<String>>,
    /// 终态动作（首个 submit 写入，`finish` 不取走）：只有真实作答才允许进重放缓存
    /// （spec duplicate-ask-coalescing D6）。
    winner_action: Mutex<Option<ChannelAction>>,
    /// 是否已进入收尾阶段（首个 submit 后置位）。GUI 据此拦下「关窗即退出」，
    /// 仅放行协调器自身的 `app.exit`，确保结果先输出；收尾前不拦（Cmd+Q 等照常退出）。
    finalizing: AtomicBool,
    /// 结果是否已输出（保证只输出 / 退出一次）。
    emitted: AtomicBool,
    /// Whether this request should be recorded in ordinary reply history.
    record_history_enabled: bool,
}

struct Inner {
    exiter: Exiter,
    request: AskRequest,
    channels: Vec<Arc<dyn Channel>>,
    /// headless 模式：共享抢答信号 + 消息渠道总数（用于算落败数）。GUI 为 None。
    headless: Option<(Arc<Preemption>, usize)>,
}

impl Coordinator {
    /// GUI 模式协调器。`project` / `source` / `agent_kind` 写入回复历史（可空）。
    pub fn new(
        app: AppHandle,
        request: AskRequest,
        project: String,
        source: String,
        agent_kind: Option<String>,
    ) -> Arc<Self> {
        let caller = crate::cli::caller_context();
        let binding = merged_caller_binding(agent_kind, caller);
        Self::build(
            Exiter::Gui(app),
            request,
            None,
            Lang::current(),
            project,
            source,
            binding.agent_kind,
            binding.agent_session_id,
            binding.mcp_instance_id,
            true,
        )
    }

    /// headless 模式协调器（无 GUI，结果到达后直接退出进程）。
    /// `preempt` 为各会话共享的抢答信号；`messaging_count` 为并行消息渠道数。
    pub fn new_headless(
        request: AskRequest,
        preempt: Arc<Preemption>,
        messaging_count: usize,
        project: String,
        source: String,
    ) -> Arc<Self> {
        let caller = crate::cli::caller_context();
        let binding = merged_caller_binding(None, caller);
        Self::build(
            Exiter::Process,
            request,
            Some((preempt, messaging_count)),
            Lang::current(),
            project,
            source,
            binding.agent_kind,
            binding.agent_session_id,
            binding.mcp_instance_id,
            true,
        )
    }

    /// Daemon 模式协调器：结果到达后渲染并经 `tx` 回传，不退出进程。
    /// `lang` 为调用方上送的界面语言（A11，使 `auto` 跟随调用方）。
    pub fn new_ipc(
        request: AskRequest,
        lang: Lang,
        tx: UnboundedSender<RenderOutcome>,
        project: String,
        source: String,
        binding: HistoryBinding,
        record_history_enabled: bool,
    ) -> Arc<Self> {
        Self::build(
            Exiter::Ipc(tx),
            request,
            None,
            lang,
            project,
            source,
            binding.agent_kind,
            binding.agent_session_id,
            binding.mcp_instance_id,
            record_history_enabled,
        )
    }

    #[allow(clippy::too_many_arguments)] // internal constructor; a params struct adds churn without clarity
    fn build(
        exiter: Exiter,
        request: AskRequest,
        headless: Option<(Arc<Preemption>, usize)>,
        lang: Lang,
        project: String,
        source: String,
        agent_kind: Option<String>,
        agent_session_id: Option<String>,
        mcp_instance_id: Option<String>,
        record_history_enabled: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                exiter,
                request,
                channels: Vec::new(),
                headless,
            }),
            terminal: FirstTerminalGate::new(),
            lang,
            project,
            source,
            agent_kind: Mutex::new(agent_kind),
            agent_session_id,
            mcp_instance_id,
            pending: Arc::new(AtomicUsize::new(0)),
            result: Mutex::new(None),
            winner: Mutex::new(None),
            winner_action: Mutex::new(None),
            finalizing: AtomicBool::new(false),
            emitted: AtomicBool::new(false),
            record_history_enabled,
        })
    }

    /// 是否已进入收尾阶段（供 GUI 事件循环决定是否拦下关窗退出）。
    pub fn is_finalizing(&self) -> bool {
        self.finalizing.load(Ordering::SeqCst)
    }

    /// 回填调用方 agent 家族（daemon 异步 walk 解析完成后调用；覆盖 env 探测值或 None）。
    pub fn set_agent_kind(&self, kind: String) {
        *self.agent_kind.lock().unwrap() = Some(kind);
    }

    #[cfg(test)]
    pub(crate) fn recovery_binding(&self) -> (Option<String>, Option<String>, Option<String>) {
        (
            self.agent_kind.lock().unwrap().clone(),
            self.agent_session_id.clone(),
            self.mcp_instance_id.clone(),
        )
    }

    pub fn register(&self, channel: Arc<dyn Channel>) {
        self.inner.lock().unwrap().channels.push(channel);
    }

    /// 是否已登记某个渠道（按 id）。用于「补推在途」时避免对同一渠道重复挂接 / 重发卡片。
    pub fn has_channel(&self, id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .channels
            .iter()
            .any(|c| c.id() == id)
    }

    /// 赢家渠道 id（终态结果的来源；未作答 / 系统取消时为 None）。供作答后把活跃槽更新为该渠道。
    pub fn winner_channel_id(&self) -> Option<String> {
        self.winner.lock().unwrap().clone()
    }

    /// 终态是不是「用户真的答了」。取消 / 系统收尾均为 false（spec duplicate-ask-coalescing D6）。
    pub fn answered(&self) -> bool {
        matches!(
            *self.winner_action.lock().unwrap(),
            Some(ChannelAction::Send)
        )
    }

    /// 投递终态结果：仅首个生效；随后取消其余 Channel 并启动收尾窗口，到时输出并退出。
    pub fn submit(self: &Arc<Self>, mut result: ChannelResult) {
        if !self.terminal.try_set(()) {
            return;
        }
        let request = { self.inner.lock().unwrap().request.clone() };
        crate::todos::apply_todo_deliveries(&request, &mut result, &self.project);
        let (exiter, pending_count, dequeue_ids) = {
            let inner = self.inner.lock().unwrap();
            // 进入收尾：此后 GUI 拦下关窗退出，独占由协调器主动 `app.exit`。
            self.finalizing.store(true, Ordering::SeqCst);
            let source = result.source_channel_id.clone();
            let action = result.action;
            // 首个终态回答＝「开始执行」时刻（spec todo-whats-next D2）：收集回答消耗的待办 id，
            // 锁外 best-effort 出队（whats-next / Stop 卡 chip + 弹窗折叠待办区）。
            let dequeue_ids = crate::todos::ids_to_dequeue(&inner.request, &result);
            *self.winner.lock().unwrap() = Some(source.clone());
            *self.winner_action.lock().unwrap() = Some(action);
            *self.result.lock().unwrap() = Some(result);

            let lang = self.lang;
            let winner = display_name(&source, lang);
            // Reason for interrupting the losing channels: a real answer (Send) attributes the
            // winner ("Answered via X"); a popup Cancel means the whole request was cancelled by
            // that source ("Cancelled by Popup").
            let reason = match action {
                ChannelAction::Send => Interruption::AnsweredBy(winner.clone()),
                ChannelAction::Cancel => Interruption::Cancelled(winner.clone()),
            };

            let pending = match &inner.headless {
                // headless：取消共享信号；落败数 = 渠道数 - 1（赢家）。
                Some((preempt, count)) => {
                    preempt.interrupt(reason.clone());
                    count.saturating_sub(1)
                }
                // GUI：逐个取消落败渠道；弹窗瞬时关闭不计入收尾等待。
                None => {
                    let losers: Vec<Arc<dyn Channel>> = inner
                        .channels
                        .iter()
                        .filter(|c| c.id() != source)
                        .cloned()
                        .collect();
                    for ch in &losers {
                        ch.interrupt(&reason);
                    }
                    losers.iter().filter(|c| c.id() != "popup").count()
                }
            };
            (inner.exiter.clone(), pending, dequeue_ids)
        };

        if !dequeue_ids.is_empty() {
            let _ = crate::todos::take(&self.project, &dequeue_ids);
        }

        self.pending.store(pending_count, Ordering::SeqCst);

        // GUI（单进程）：立即关闭弹窗（赢家是弹窗时它不在 losers 中，需显式关）。
        // Daemon 模式弹窗在独立 GUI Helper 进程，关窗由其自身收到 cancel / 连接断开处理，此处不涉及。
        if let Exiter::Gui(app) = &exiter {
            if let Some(w) = app.get_webview_window("popup") {
                let _ = w.close();
            }
        }

        // 收尾窗口：等落败端收尾完成（pending 归零）或 2s 超时后输出并退出。
        let me = Arc::clone(self);
        let waiter = async move {
            me.wait_for_finalizers().await;
            me.finish();
        };
        match exiter {
            Exiter::Gui(_) => {
                tauri::async_runtime::spawn(waiter);
            }
            Exiter::Process | Exiter::Ipc(_) => {
                tokio::spawn(waiter);
            }
        }
    }

    /// Cancel the whole request (CLI disconnected / `daemon stop`): interrupt every channel as
    /// `Cancelled(source)` so all cards finalize to a cancelled state and the popup closes.
    /// Unlike `submit`, this does not render or deliver a result (no one is waiting), but it does
    /// preserve the original prompt in reply history. No-op if a result was already submitted.
    /// `source` is the localized cancel source; `source_channel_id` is its stable history id.
    pub fn cancel_request(&self, source: String, source_channel_id: &str) {
        if !self.terminal.try_set(()) {
            return;
        }
        let inner = self.inner.lock().unwrap();
        let pending_count = match &inner.headless {
            Some((_, count)) => *count,
            None => inner
                .channels
                .iter()
                .filter(|channel| channel.id() != "popup")
                .count(),
        };
        // Caller disconnect cancellation has no result waiter to keep the request handler alive.
        // Count IM finalizers before interrupting them so the daemon can retain the coordinator
        // until every card reaches a terminal state (or the bounded timeout expires).
        self.pending.store(pending_count, Ordering::SeqCst);
        let reason = Interruption::Cancelled(source);
        match &inner.headless {
            Some((preempt, _)) => preempt.interrupt(reason),
            None => {
                for ch in &inner.channels {
                    ch.interrupt(&reason);
                }
            }
        }
        // A disconnected caller still created a real request. Keep its prompt and predefined
        // options in history just like a human-initiated Cancel; only answers are empty.
        self.record_history(
            &inner.request,
            &ChannelResult::cancel(source_channel_id),
            &[],
        );
    }

    /// Wait for interrupted IM channels to finish their terminal card updates.
    ///
    /// This is shared by normal first-answer finalization and caller-disconnect cancellation.
    /// The timeout keeps a broken network integration from retaining a daemon request forever.
    pub async fn wait_for_finalizers(&self) -> bool {
        let deadline = Instant::now() + FINALIZE_TIMEOUT;
        while self.pending.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.pending.load(Ordering::SeqCst) == 0
    }

    /// 一个落败渠道完成收尾时调用：未归零则减一（用于提前结束收尾窗口）。
    pub fn notify_finalized(&self) {
        let _ = self
            .pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| {
                if v > 0 {
                    Some(v - 1)
                } else {
                    None
                }
            });
    }

    /// 输出已采纳结果并退出（只生效一次）。无结果时直接返回，交由调用方兜底。
    pub fn finish(&self) {
        if self.emitted.swap(true, Ordering::SeqCst) {
            return;
        }
        let (exiter, request) = {
            let inner = self.inner.lock().unwrap();
            (inner.exiter.clone(), inner.request.clone())
        };
        let result = self.result.lock().unwrap().take();
        let Some(result) = result else {
            // 无结果（headless 全部会话结束仍未作答）：不退出，交由调用方报错。
            return;
        };
        // 渲染一次（图片落盘只发生一次），拿到给 CLI 的文本与各题图片路径。
        let (outcome, image_paths) = super::render_result(&request, &result, self.lang);
        // 旁路写回复历史：最佳努力，绝不影响主流程（stdout / 退出码）。
        self.record_history(&request, &result, &image_paths);
        // Daemon 模式：回传连接处理器，不打印、不退出（进程常驻）。
        if let Exiter::Ipc(tx) = &exiter {
            let _ = tx.send(outcome);
            return;
        }
        // 单进程：打印结果并退出。
        if let Some(err) = &outcome.stderr {
            super::stderr_redirect::eprintln_real(err);
        } else {
            println!("{}", outcome.stdout);
        }
        let _ = std::io::stdout().flush();
        match exiter {
            Exiter::Gui(app) => app.exit(outcome.exit_code),
            Exiter::Process => std::process::exit(outcome.exit_code),
            Exiter::Ipc(_) => unreachable!("handled above"),
        }
    }

    /// Append a reply-history entry for this terminal result (best-effort side channel).
    ///
    /// `finish` records user-initiated terminal results; caller/system cancellation records the
    /// same request snapshot through `cancel_request`. Image/file values are stored as paths.
    fn record_history(
        &self,
        request: &AskRequest,
        result: &ChannelResult,
        image_paths: &[Vec<String>],
    ) {
        if !self.record_history_enabled {
            return;
        }
        // 仅需 history_limit（general）；用 load_without_secrets() 避免每条回答落历史都读钥匙串。
        let limit = crate::config::AppConfig::load_without_secrets()
            .general
            .history_limit;
        if limit == 0 {
            return;
        }
        let entry = self.history_entry(request, result, image_paths, crate::history::now_ms());
        crate::history::record(entry, limit);
    }

    fn history_entry(
        &self,
        request: &AskRequest,
        result: &ChannelResult,
        image_paths: &[Vec<String>],
        timestamp_ms: i64,
    ) -> crate::history::HistoryEntry {
        let answers = match result.action {
            ChannelAction::Cancel => Vec::new(),
            ChannelAction::Send => result
                .answers
                .iter()
                .enumerate()
                .map(|(i, a)| crate::history::HistoryAnswer {
                    selected_options: a.selected_options.clone(),
                    user_input: a.user_input.clone(),
                    images: image_paths.get(i).cloned().unwrap_or_default(),
                    files: a.files.clone(),
                })
                .collect(),
        };
        crate::history::HistoryEntry {
            id: request.id.clone(),
            timestamp_ms,
            project: self.project.clone(),
            source: self.source.clone(),
            agent_kind: self.agent_kind.lock().unwrap().clone(),
            agent_session_id: self.agent_session_id.clone(),
            mcp_instance_id: self.mcp_instance_id.clone(),
            channel: result.source_channel_id.clone(),
            action: result.action,
            is_markdown: request.is_markdown,
            message: request.message.clone(),
            questions: request.questions.clone(),
            answers,
        }
    }
}

fn merged_caller_binding(
    explicit_agent_kind: Option<String>,
    caller: crate::cli::CallerContext,
) -> HistoryBinding {
    HistoryBinding {
        agent_kind: explicit_agent_kind.or(caller.agent_kind),
        agent_session_id: caller.agent_session_id,
        mcp_instance_id: caller.mcp_instance_id,
    }
}

/// 渠道 id → 赢家端展示名（按界面语言）。
fn display_name(id: &str, lang: Lang) -> String {
    match id {
        "popup" => i18n::tr(lang, "channel.sourcePopup").to_string(),
        "telegram" => i18n::tr(lang, "channel.sourceTelegram").to_string(),
        "dingding" => i18n::tr(lang, "channel.sourceDingTalk").to_string(),
        "feishu" => i18n::tr(lang, "channel.sourceFeishu").to_string(),
        "slack" => i18n::tr(lang, "channel.sourceSlack").to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ConversationOrigin;
    use crate::models::{MessagePrompt, Question, QuestionAnswer};

    struct InterruptChannel {
        id: &'static str,
        interrupted: Arc<AtomicBool>,
    }

    impl Channel for InterruptChannel {
        fn id(&self) -> &str {
            self.id
        }

        fn start(
            &self,
            _request: &AskRequest,
            _origin: &ConversationOrigin,
            _sink: Arc<Coordinator>,
        ) {
        }

        fn interrupt(&self, _reason: &Interruption) {
            self.interrupted.store(true, Ordering::SeqCst);
        }
    }

    fn coordinator() -> Arc<Coordinator> {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Coordinator::new_ipc(
            AskRequest::new(
                MessagePrompt::new("context".into(), Vec::new()),
                vec![Question::new("question".into(), Vec::new())],
                true,
            ),
            Lang::En,
            tx,
            "/project".into(),
            "Codex".into(),
            HistoryBinding {
                agent_kind: Some("codex".into()),
                agent_session_id: Some("session-1".into()),
                mcp_instance_id: Some("instance-1".into()),
            },
            true,
        )
    }

    #[test]
    fn history_entry_preserves_exact_agent_and_mcp_binding() {
        let coordinator = coordinator();
        let request = coordinator.inner.lock().unwrap().request.clone();
        let result = ChannelResult {
            action: ChannelAction::Send,
            answers: vec![QuestionAnswer {
                selected_options: vec!["Yes".into()],
                user_input: Some("details".into()),
                images: Vec::new(),
                files: vec!["/tmp/file.txt".into()],
                todo_ids: Vec::new(),
                todo_selections: Vec::new(),
            }],
            source_channel_id: "popup".into(),
        };
        let entry =
            coordinator.history_entry(&request, &result, &[vec!["/tmp/image.png".into()]], 123);
        assert_eq!(entry.timestamp_ms, 123);
        assert_eq!(entry.agent_kind.as_deref(), Some("codex"));
        assert_eq!(entry.agent_session_id.as_deref(), Some("session-1"));
        assert_eq!(entry.mcp_instance_id.as_deref(), Some("instance-1"));
        assert_eq!(entry.answers[0].selected_options, ["Yes"]);
        assert_eq!(entry.answers[0].images, ["/tmp/image.png"]);
        assert_eq!(entry.answers[0].files, ["/tmp/file.txt"]);

        coordinator.set_agent_kind("cursor".into());
        let updated = coordinator.history_entry(&request, &result, &[], 124);
        assert_eq!(updated.agent_kind.as_deref(), Some("cursor"));
        assert_eq!(updated.agent_session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn cancelled_history_entry_keeps_binding_but_never_answers() {
        let coordinator = coordinator();
        let request = coordinator.inner.lock().unwrap().request.clone();
        let mut result = ChannelResult::cancel("popup");
        result.answers.push(QuestionAnswer::default());
        let entry = coordinator.history_entry(&request, &result, &[], 123);
        assert_eq!(entry.action, ChannelAction::Cancel);
        assert!(entry.answers.is_empty());
        assert_eq!(entry.agent_session_id.as_deref(), Some("session-1"));
        assert_eq!(entry.mcp_instance_id.as_deref(), Some("instance-1"));
    }

    #[test]
    fn coordinator_bindings_keep_cross_platform_caller_context() {
        let caller = crate::cli::CallerContext {
            agent_kind: Some("codex".into()),
            agent_session_id: Some("thread".into()),
            mcp_instance_id: Some("instance".into()),
            from_mcp: true,
        };
        assert_eq!(
            merged_caller_binding(None, caller.clone()),
            HistoryBinding {
                agent_kind: Some("codex".into()),
                agent_session_id: Some("thread".into()),
                mcp_instance_id: Some("instance".into()),
            }
        );
        assert_eq!(
            merged_caller_binding(Some("cursor".into()), caller),
            HistoryBinding {
                agent_kind: Some("cursor".into()),
                agent_session_id: Some("thread".into()),
                mcp_instance_id: Some("instance".into()),
            }
        );
    }

    #[tokio::test]
    async fn caller_cancellation_waits_for_im_finalizers_but_not_popup() {
        let coordinator = coordinator();
        let popup_interrupted = Arc::new(AtomicBool::new(false));
        let im_interrupted = Arc::new(AtomicBool::new(false));
        coordinator.register(Arc::new(InterruptChannel {
            id: "popup",
            interrupted: popup_interrupted.clone(),
        }));
        coordinator.register(Arc::new(InterruptChannel {
            id: "feishu",
            interrupted: im_interrupted.clone(),
        }));

        coordinator.cancel_request("Caller".into(), "caller");

        assert!(popup_interrupted.load(Ordering::SeqCst));
        assert!(im_interrupted.load(Ordering::SeqCst));
        assert_eq!(coordinator.pending.load(Ordering::SeqCst), 1);
        coordinator.notify_finalized();
        assert!(coordinator.wait_for_finalizers().await);
    }
}
