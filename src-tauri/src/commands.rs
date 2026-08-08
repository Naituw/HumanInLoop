//! 前端可调用的 Tauri 命令（弹窗模式）。

use crate::app::coordinator::Coordinator;
use crate::app::AppState;
use crate::config::{AppConfig, ThemeMode, WindowEffect};
use crate::integrations::cursor_hook;
use crate::models::{ChannelAction, ChannelResult, InteractionRequest, QuestionAnswer};
use crate::telegram::TelegramClient;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager, State};

/// 弹窗初始化负载：请求内容 + 主题 + 是否置顶（前端据此套用样式、初始化导航栏）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PopupInit {
    /// Current interaction. A prewarmed helper returns `None` until it is assigned.
    interaction: Option<InteractionRequest>,
    /// Native edit intent used only by the local permission popup.
    popup_edit: Option<Box<crate::permission_diff::PermissionEditIntent>>,
    theme: String,
    always_on_top: bool,
    /// 标题来源名：「Question from {source_name}」。可经环境变量定制。
    source_name: String,
    /// 来源 workspace 完整路径（git 仓库根 / 回退 cwd）；hover 标题区显示用。空则前端隐藏该元素。
    project: String,
    /// workspace 目录名（`project` 的 basename），标题区展示用。
    project_name: String,
    /// 发起本次提问的 agent 家族（claude/codex/cursor/grok）；None 则不显示 agent badge。
    agent_kind: Option<String>,
    /// 发起本次提问的 agent 进程 pid；前端「聚焦终端」用。
    agent_pid: Option<u32>,
    /// 已严格匹配到 Agent 状态窗口活动记录的会话 ID；None 时不显示快捷入口。
    agent_console_session_id: Option<String>,
    /// 界面语言原始值（`auto`/`en`/`zh`）。让弹窗直接据此 `applyLanguage`，免去前端再走 `get_settings()`
    /// （钥匙串）。`auto` 由前端解析为系统语言。
    language: String,
    /// 语音识别语言（BCP-47，如 `zh-CN`；`auto` 跟随系统）。来自内存态配置，无钥匙串。
    speech_language: String,
    /// 语音输入快捷键（规范串如 `cmd+d`；空串=关闭）。来自内存态配置，无钥匙串。
    speech_shortcut: String,
    /// 提交快捷键：`cmdEnter`（⌘/Ctrl+Enter）或 `enter`（裸 Enter）。
    popup_submit_key: String,
    /// 实验：多问题弹窗是否纵向同时显示所有问题（默认关 = 旧版一次一题）。
    vertical_questions: bool,
    /// 性能埋点是否开启（helper 进程收到了 `ASKHUMAN_PERF_ID`）；前端据此决定是否上报 perf 标记。
    perf: bool,
    /// 性能测试：画完首帧后自动取消弹窗（仅 harness 用）。
    perf_autodismiss: bool,
    /// 方案6：本进程是否为预热弹窗（窗口起始隐藏）。为真时前端在内容绘制完成后调 `popup_show_window`
    /// 让后端上屏（延后 show）；冷路径为假（窗口已在 setup 中显示）。
    warm: bool,
    /// 提问创建时刻（epoch 毫秒）：弹窗据此显示相对时间（几秒/分钟/小时前），超过一天显示绝对时间。
    /// 0 表示未知（非弹窗窗口）。
    created_at_ms: u64,
}

#[tauri::command]
pub fn popup_init(app: AppHandle, state: State<AppState>) -> PopupInit {
    // 方案6 预热弹窗：内容来自领用槽（`WarmPopup.show`）——`Some`=已领用、`None`=待命（request 返回 null，
    // 前端等 `popup-show` 唤醒后再 pull）。冷 / 单进程：内容在构建时已注入 `AppState`。
    // language：预热弹窗进程长期存活、`state.config` 可能滞后，故领用时优先用 `Show.lang`（已解析的
    // en/zh）；其余路径用本进程 config 的原始值（auto/en/zh）。
    let default_lang = state.config.general.language.clone();
    #[cfg(unix)]
    let (
        interaction,
        popup_edit,
        source,
        project,
        agent_kind,
        agent_pid,
        agent_console_session_id,
        language,
        warm,
        created_at_ms,
    ) = if let Some(w) = app.try_state::<crate::app::WarmPopup>() {
        match w.show.lock().ok().and_then(|g| g.clone()) {
            Some(s) => (
                Some(s.interaction),
                s.popup_edit,
                s.source,
                s.project,
                s.agent_kind,
                s.agent_pid,
                s.agent_console_session_id,
                s.lang,
                true,
                s.created_at_ms,
            ),
            None => (
                None,
                None,
                String::new(),
                String::new(),
                None,
                None,
                None,
                default_lang,
                true,
                0,
            ),
        }
    } else {
        (
            Some(state.interaction.clone()),
            state.popup_edit.clone(),
            state.source.clone(),
            state.project.clone(),
            state.agent_kind.clone(),
            state.agent_pid,
            state.agent_console_session_id.clone(),
            default_lang,
            false,
            state.created_at_ms,
        )
    };
    #[cfg(not(unix))]
    let (
        interaction,
        popup_edit,
        source,
        project,
        agent_kind,
        agent_pid,
        agent_console_session_id,
        language,
        warm,
        created_at_ms,
    ) = (
        Some(state.interaction.clone()),
        state.popup_edit.clone(),
        state.source.clone(),
        state.project.clone(),
        state.agent_kind.clone(),
        state.agent_pid,
        state.agent_console_session_id.clone(),
        default_lang,
        false,
        state.created_at_ms,
    );
    let _ = &app;

    // 预热进程长存、`state.config` 可能滞后：领用时按最新 config 取主题/置顶/语音（无钥匙串）；
    // 其余路径（刚 spawn 的冷 helper / 单进程）用本进程 config 即可。
    let fresh = if warm {
        Some(AppConfig::load_without_secrets())
    } else {
        None
    };
    let cfg = fresh.as_ref().unwrap_or(&state.config);

    let project_name = crate::project::display_name(&project);
    PopupInit {
        interaction,
        popup_edit,
        theme: theme_str(cfg.general.theme),
        always_on_top: cfg.general.always_on_top,
        // GUI Helper 模式下来源名由 Daemon 上送（A11）；单进程 / 设置回退取本进程环境。
        source_name: source,
        project,
        project_name,
        agent_kind,
        agent_pid,
        agent_console_session_id,
        language,
        speech_language: cfg.general.speech_language.clone(),
        speech_shortcut: cfg.general.speech_shortcut.clone(),
        popup_submit_key: cfg.general.popup_submit_key.as_str().to_string(),
        vertical_questions: cfg.experimental.vertical_questions,
        perf: !crate::perf::effective_id().is_empty(),
        perf_autodismiss: crate::perf::autodismiss(),
        warm,
        created_at_ms,
    }
}

#[tauri::command]
pub async fn enrich_permission_diff(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: String,
) -> Result<crate::permission_diff::PermissionDiffModel, String> {
    #[cfg(unix)]
    let (current_id, intent) = if let Some(warm) = app.try_state::<crate::app::WarmPopup>() {
        let show = warm
            .show
            .lock()
            .map_err(|_| "permission diff state unavailable".to_string())?
            .clone()
            .ok_or_else(|| "permission diff request is not assigned".to_string())?;
        (show.request_id, show.popup_edit)
    } else {
        let id = state
            .interaction
            .confirm()
            .map(|request| request.id.clone())
            .ok_or_else(|| "permission diff requires a confirmation".to_string())?;
        (id, state.popup_edit.clone())
    };
    #[cfg(not(unix))]
    let (current_id, intent) = {
        let _ = &app;
        let id = state
            .interaction
            .confirm()
            .map(|request| request.id.clone())
            .ok_or_else(|| "permission diff requires a confirmation".to_string())?;
        (id, state.popup_edit.clone())
    };

    if current_id != request_id {
        return Err("permission diff request changed".to_string());
    }
    let intent = *intent.ok_or_else(|| "permission diff is unavailable".to_string())?;
    let paths = crate::permission_diff::safety::operation_paths(&intent);
    if paths.is_empty() {
        return Ok(crate::permission_diff::worker::fallback_model(
            &intent,
            &request_id,
            crate::permission_diff::SnapshotStatus::Unsupported,
        ));
    }
    let protected_paths = crate::permission_diff::safety::protected_paths(&intent);
    if protected_paths.len() >= paths.len() {
        return Ok(crate::permission_diff::worker::fallback_model(
            &intent,
            &request_id,
            crate::permission_diff::SnapshotStatus::ProtectedPath,
        ));
    }
    let input = crate::permission_diff::PermissionDiffWorkerInput {
        request_id: request_id.clone(),
        intent: intent.clone(),
        protected_paths,
    };
    match crate::permission_diff::worker::spawn_worker(input).await {
        Ok(output) if output.request_id == request_id => Ok(output.model),
        Ok(_) => Err("permission diff worker returned a stale result".to_string()),
        Err(status) => Ok(crate::permission_diff::worker::fallback_model(
            &intent,
            &request_id,
            status,
        )),
    }
}

/// Report that popup content and its hidden native window are ready. The daemon replies with the
/// authoritative foreground/background presentation after cross-process focus arbitration.
#[tauri::command]
pub fn popup_show_window(app: AppHandle) {
    #[cfg(unix)]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            let window_number = {
                #[cfg(target_os = "macos")]
                {
                    app2.get_webview_window("popup")
                        .and_then(|window| window.ns_window().ok())
                        .and_then(crate::macos_window_order::window_number)
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
            };
            if let Some(bridge) = app2.try_state::<crate::app::GuiBridge>() {
                bridge.send_popup_ready(window_number);
            }
        });
    }
    #[cfg(not(unix))]
    let _ = app;
}

/// 前端性能埋点回传：把某阶段标记写入 `perf.log`（关联 id 取自 helper 进程的 `ASKHUMAN_PERF_ID`）。
/// `ts` 为前端 `Date.now()`（epoch ms），使时间反映页面而非 IPC 往返；省略则用当前时间。
/// 埋点关闭（无关联 id）时为 no-op。
#[tauri::command]
pub fn perf_mark(stage: String, ts: Option<f64>) {
    let id = crate::perf::effective_id();
    match ts {
        Some(t) => crate::perf::mark_at(&id, &stage, t as u128),
        None => crate::perf::mark(&id, &stage),
    }
}

/// 解析某 agent pid 所在终端类型（`apple-terminal`/`iterm2`/…）。**刻意独立于 `popup_init` + 异步**：
/// `terminal_kind` 要沿进程链跑多次 `ps`（数十毫秒级），放进 `popup_init` 会拖慢弹窗首屏、露出「加载中」。
/// 方案5(b) 起 agent pid 由 daemon 异步 walk 后经 `agent-resolved` 事件下发（不再随 `popup_init`），前端
/// 拿到 pid 后调用本命令把 badge 升级成「可点 + ↗」。`pid==0` / 探测不到 → None。
#[tauri::command]
pub fn popup_agent_terminal(pid: u32) -> Option<String> {
    if pid == 0 {
        return None;
    }
    crate::agents::detect::terminal_kind(pid).map(|s| s.to_string())
}

/// 调用方 agent 的异步解析结果（方案5/b）：daemon 经 `AgentResolved` 后推、GUI Helper 缓存进程内一份，
/// 供弹窗挂载时拉取初值（规避「事件早于前端监听」竞态，与自更新态同模式）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushedAgent {
    pub kind: Option<String>,
    pub pid: Option<u32>,
}

static PUSHED_AGENT: std::sync::OnceLock<std::sync::Mutex<PushedAgent>> =
    std::sync::OnceLock::new();

fn pushed_agent_slot() -> &'static std::sync::Mutex<PushedAgent> {
    PUSHED_AGENT.get_or_init(|| std::sync::Mutex::new(PushedAgent::default()))
}

/// GUI Helper 收到 daemon 的 `AgentResolved` 后写入此缓存（供弹窗挂载拉取初值 + 后续事件实时更新）。
pub fn set_pushed_agent(agent: PushedAgent) {
    if let Ok(mut slot) = pushed_agent_slot().lock() {
        *slot = agent;
    }
}

/// 弹窗挂载时拉取「已推送的调用方 agent 解析结果」初值（之后变化经 `agent-resolved` 事件实时更新）。
#[tauri::command]
pub fn popup_agent_resolved() -> PushedAgent {
    pushed_agent_slot()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

// ===== 项目级待办队列（spec todo-whats-next D7/D9）：直读直写 todos.json，无 daemon 依赖 =====

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoAttachmentView {
    id: String,
    name: String,
    size: u64,
    is_image: bool,
    source_path: String,
    path: String,
    storage: crate::todo_attachments::TodoAttachmentStorage,
    available: bool,
}

impl From<&crate::todo_attachments::TodoAttachment> for TodoAttachmentView {
    fn from(attachment: &crate::todo_attachments::TodoAttachment) -> Self {
        Self {
            id: attachment.id.clone(),
            name: attachment.name.clone(),
            size: attachment.size,
            is_image: attachment.is_image,
            source_path: attachment.source_path.clone(),
            path: attachment.path.clone(),
            storage: attachment.storage,
            available: attachment.available(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoEntryView {
    id: String,
    text: String,
    created_at_ms: u64,
    agent_kind: Option<String>,
    auto: bool,
    attachments: Vec<TodoAttachmentView>,
}

impl From<crate::todos::TodoEntry> for TodoEntryView {
    fn from(entry: crate::todos::TodoEntry) -> Self {
        Self {
            id: entry.id,
            text: entry.text,
            created_at_ms: entry.created_at_ms,
            agent_kind: entry.agent_kind,
            auto: entry.auto,
            attachments: entry
                .attachments
                .iter()
                .map(TodoAttachmentView::from)
                .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneTodoEntryView {
    id: String,
    text: String,
    created_at_ms: u64,
    agent_kind: Option<String>,
    done_at_ms: u64,
    attachments: Vec<TodoAttachmentView>,
}

impl From<crate::todos::DoneTodoEntry> for DoneTodoEntryView {
    fn from(entry: crate::todos::DoneTodoEntry) -> Self {
        Self {
            id: entry.id,
            text: entry.text,
            created_at_ms: entry.created_at_ms,
            agent_kind: entry.agent_kind,
            done_at_ms: entry.done_at_ms,
            attachments: entry
                .attachments
                .iter()
                .map(TodoAttachmentView::from)
                .collect(),
        }
    }
}

#[tauri::command]
pub fn todos_list(project: String) -> Vec<TodoEntryView> {
    crate::todos::list(&project)
        .into_iter()
        .map(TodoEntryView::from)
        .collect()
}

#[tauri::command]
pub async fn todos_add(
    project: String,
    text: String,
    auto: Option<bool>,
    file_paths: Option<Vec<String>>,
    pasted_images: Option<Vec<crate::models::ImageAttachment>>,
) -> Result<TodoEntryView, String> {
    let mut file_paths = file_paths.unwrap_or_default();
    let pasted_images = pasted_images.unwrap_or_default();
    validate_pasted_images(&pasted_images)?;
    if file_paths.len().saturating_add(pasted_images.len())
        > crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO
    {
        return Err(format!(
            "a todo can have at most {} attachments",
            crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO
        ));
    }
    let pasted_images = normalize_pasted_image_filenames(pasted_images);
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    tauri::async_runtime::spawn_blocking(move || {
        let paste_id = format!("todo-paste-{}", uuid::Uuid::new_v4());
        let paste_root = crate::paths::request_temp_dir(&paste_id);
        let result = (|| {
            file_paths.extend(save_pasted_images(&pasted_images, &paste_id)?);
            crate::todos::add_with_attachments(
                &project,
                &text,
                auto.unwrap_or(false),
                None,
                &file_paths,
                &cwd,
            )
            .map(TodoEntryView::from)
            .map_err(|error| match error {
                crate::todos::AddError::EmptyInput => "todo text must not be empty".to_string(),
                crate::todos::AddError::Persist => "failed to save todo".to_string(),
                crate::todos::AddError::Attachment(message) => message,
            })
        })();
        let _ = std::fs::remove_dir_all(paste_root);
        result
    })
    .await
    .map_err(|error| format!("todo add task failed: {error}"))?
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn todos_update(
    project: String,
    id: String,
    expected_text: String,
    expected_attachment_ids: Vec<String>,
    text: String,
    keep_attachment_ids: Vec<String>,
    add_paths: Vec<String>,
) -> Result<TodoEntryView, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    tauri::async_runtime::spawn_blocking(move || {
        crate::todos::update_with_attachments(
            &project,
            &id,
            &expected_text,
            &expected_attachment_ids,
            &text,
            &keep_attachment_ids,
            &add_paths,
            &cwd,
        )
        .map(TodoEntryView::from)
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("todo update task failed: {error}"))?
}

/// Add or remove attachments without entering text-edit mode.
#[tauri::command]
pub async fn todos_update_attachments(
    project: String,
    id: String,
    add_paths: Vec<String>,
    remove_attachment_ids: Vec<String>,
) -> Result<TodoEntryView, String> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    tauri::async_runtime::spawn_blocking(move || {
        crate::todos::update_attachment_ids(&project, &id, &add_paths, &remove_attachment_ids, &cwd)
            .map(TodoEntryView::from)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| format!("todo attachment update task failed: {error}"))?
}

/// Give clipboard images stable unique names before writing them into one temporary directory.
fn normalize_pasted_image_filenames(
    images: Vec<crate::models::ImageAttachment>,
) -> Vec<crate::models::ImageAttachment> {
    images
        .into_iter()
        .enumerate()
        .map(|(index, mut image)| {
            image.filename = image.filename.as_deref().and_then(|name| {
                let basename = name.rsplit(['/', '\\']).next()?.trim();
                (!basename.is_empty()).then(|| format!("{}-{basename}", index + 1))
            });
            image
        })
        .collect()
}

fn validate_pasted_images(images: &[crate::models::ImageAttachment]) -> Result<(), String> {
    const DATA_URL_OVERHEAD_BYTES: usize = 1024;
    const MAX_ENCODED_IMAGE_BYTES: usize =
        (crate::todo_attachments::MANAGED_MAX_BYTES as usize).div_ceil(3) * 4
            + DATA_URL_OVERHEAD_BYTES;

    if images.len() > crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO {
        return Err(format!(
            "a todo can have at most {} attachments",
            crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO
        ));
    }
    if images.iter().any(|image| {
        !image.media_type.starts_with("image/") || image.data.len() > MAX_ENCODED_IMAGE_BYTES
    }) {
        return Err(format!(
            "a pasted image must be at most {} bytes",
            crate::todo_attachments::MANAGED_MAX_BYTES
        ));
    }
    Ok(())
}

fn save_pasted_images(
    images: &[crate::models::ImageAttachment],
    paste_id: &str,
) -> Result<Vec<String>, String> {
    let paths = crate::cli::image_writer::save(images, paste_id, 0, crate::i18n::Lang::current())
        .map_err(|error| error.to_string())?;
    for path in &paths {
        let size = std::fs::metadata(path)
            .map_err(|error| format!("failed to inspect pasted image: {error}"))?
            .len();
        if size > crate::todo_attachments::MANAGED_MAX_BYTES {
            return Err(format!(
                "a pasted image must be at most {} bytes",
                crate::todo_attachments::MANAGED_MAX_BYTES
            ));
        }
    }
    Ok(paths)
}

/// Persist clipboard images into the selected todo. Clipboard blobs have no stable source path,
/// so they must fit the managed-file limit rather than degrading to a temporary path reference.
#[tauri::command]
pub async fn todos_attach_pasted_images(
    project: String,
    id: String,
    images: Vec<crate::models::ImageAttachment>,
) -> Result<TodoEntryView, String> {
    if images.is_empty() {
        return Err("no clipboard images supplied".to_string());
    }
    validate_pasted_images(&images)?;
    let images = normalize_pasted_image_filenames(images);

    tauri::async_runtime::spawn_blocking(move || {
        let paste_id = format!("todo-paste-{}", uuid::Uuid::new_v4());
        let paste_root = crate::paths::request_temp_dir(&paste_id);
        let result = (|| {
            let paths = save_pasted_images(&images, &paste_id)?;
            crate::todos::update_attachment_ids(
                &project,
                &id,
                &paths,
                &[],
                &std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            )
            .map(TodoEntryView::from)
            .map_err(|error| error.to_string())
        })();
        let _ = std::fs::remove_dir_all(paste_root);
        result
    })
    .await
    .map_err(|error| format!("todo clipboard image task failed: {error}"))?
}

#[tauri::command]
pub async fn todo_attachment_thumbnail(
    project: String,
    todo_id: String,
    attachment_id: String,
) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let pending = crate::todos::list(&project)
            .into_iter()
            .find(|entry| entry.id == todo_id)
            .map(|entry| entry.attachments);
        let attachments = pending.or_else(|| {
            crate::todos::history(&project)
                .into_iter()
                .find(|entry| entry.id == todo_id)
                .map(|entry| entry.attachments)
        })?;
        let attachment = attachments
            .iter()
            .find(|attachment| attachment.id == attachment_id)?;
        crate::todo_attachments::read_thumbnail_data_url(&todo_id, attachment)
    })
    .await
    .ok()
    .flatten()
}

/// 切换自动执行标记（第 17 轮定案）；返回新状态，条目不存在返回 None。
#[tauri::command]
pub fn todos_set_auto(project: String, id: String, auto: bool) -> Option<bool> {
    crate::todos::set_auto(&project, &id, auto)
}

/// 修改待办正文（GUI 双击编辑）；返回落盘后的文本，条目不存在或空文本返回 None。
#[tauri::command]
pub fn todos_set_text(project: String, id: String, text: String) -> Option<String> {
    crate::todos::set_text(&project, &id, &text)
}

#[tauri::command]
pub fn todos_remove(project: String, id: String) -> Result<bool, String> {
    crate::todos::remove_checked(&project, &id).map_err(|error| error.to_string())
}

/// GUI checkbox complete: dequeue into execution history (same path as whats-next `take`).
/// Returns whether the entry existed and was moved.
#[tauri::command]
pub fn todos_complete(project: String, id: String) -> bool {
    !crate::todos::take(&project, &[id]).is_empty()
}

#[tauri::command]
pub fn todos_clear(project: String) -> Result<usize, String> {
    crate::todos::clear_checked(&project).map_err(|error| error.to_string())
}

/// 拖拽排序（GUI 待办窗口，第 14 轮定案）：按给定 id 顺序重排；并发增删 best-effort。
#[tauri::command]
pub fn todos_reorder(project: String, ids: Vec<String>) -> bool {
    crate::todos::reorder(&project, &ids)
}

/// 执行历史（第 16 轮定案）：最新在前。
#[tauri::command]
pub fn todos_history(project: String) -> Vec<DoneTodoEntryView> {
    crate::todos::history(&project)
        .into_iter()
        .map(DoneTodoEntryView::from)
        .collect()
}

/// 从历史一键恢复回待办队列末尾。
#[tauri::command]
pub fn todos_restore(project: String, id: String) -> bool {
    crate::todos::restore(&project, &id)
}

/// 清空本项目的执行历史（第 18 轮定案）。
#[tauri::command]
pub fn todos_history_clear(project: String) -> Result<usize, String> {
    crate::todos::clear_history_checked(&project).map_err(|error| error.to_string())
}

/// 待办窗口初始化负载：主题 + 语言（与 `agents_init` 同模式）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodosInit {
    theme: String,
    lang: String,
    /// 与弹窗一致的提交快捷键：`cmdEnter`（⌘/Ctrl+Enter）或 `enter`（裸 Enter）。
    popup_submit_key: String,
    /// 「创建任务」入口是否可用（spec gui-agent-task-launch G1）：macOS 且 Terminal.app 存在。
    new_task_supported: bool,
}

#[tauri::command]
pub fn todos_init() -> TodosInit {
    // 现读配置而非 `AppState.config`：GUI Host 常驻，进程级快照会滞后于设置变更，
    // 重开窗口时会拿到旧主题/语言（原生窗口底色已切、内容却停在旧主题）。
    let config = AppConfig::load_without_secrets();
    TodosInit {
        theme: theme_str(config.general.theme),
        lang: crate::i18n::Lang::resolve(&config.general.language)
            .code()
            .to_string(),
        popup_submit_key: config.general.popup_submit_key.as_str().to_string(),
        new_task_supported: crate::integrations::agent_launch::terminal_available(),
    }
}

/// 待办窗口项目选择器候选（spec todo-whats-next D9）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoProjectInfo {
    /// 项目 key（git 根路径）。
    pub key: String,
    /// 显示名（basename）。
    pub name: String,
    /// 该项目当前待办条数。
    pub count: usize,
    /// 选择器分组：`withTodos`（有待办）或 `recent`（最近工作过；与 IM `/todo` 候选同源）。
    pub section: String,
}

/// Intermediate candidate shared by the two GUI selector sections.
/// Ranking mirrors IM `/todo` project pick (working → idle → pinned → last used).
struct GuiTodoProjectCandidate {
    key: String,
    pinned: bool,
    last_used_at: u64,
    has_workspace: bool,
    /// 0 = working, 1 = idle, 2 = no live Agent.
    activity_rank: u8,
    todo_count: usize,
}

fn gui_todo_project_sort(
    a: &GuiTodoProjectCandidate,
    b: &GuiTodoProjectCandidate,
) -> std::cmp::Ordering {
    a.activity_rank
        .cmp(&b.activity_rank)
        .then_with(|| b.pinned.cmp(&a.pinned))
        .then_with(|| b.has_workspace.cmp(&a.has_workspace))
        .then_with(|| b.last_used_at.cmp(&a.last_used_at))
        .then_with(|| {
            crate::project::display_name(&a.key)
                .to_lowercase()
                .cmp(&crate::project::display_name(&b.key).to_lowercase())
        })
        .then_with(|| a.key.cmp(&b.key))
}

fn build_todo_project_list(agents: Option<&serde_json::Value>) -> Vec<TodoProjectInfo> {
    let todos = crate::todos::all();
    let mut by_key: std::collections::HashMap<String, GuiTodoProjectCandidate> =
        std::collections::HashMap::new();

    // 最近 workspace 索引（隐藏项不列）——本地文件，瞬时。
    for workspace in crate::agents::workspaces::list()
        .into_iter()
        .filter(|workspace| !workspace.hidden)
    {
        let key = crate::project::detect_from(std::path::Path::new(&workspace.path));
        if key.is_empty() {
            continue;
        }
        let entry = by_key
            .entry(key.clone())
            .or_insert_with(|| GuiTodoProjectCandidate {
                key,
                pinned: false,
                last_used_at: 0,
                has_workspace: true,
                activity_rank: 2,
                todo_count: 0,
            });
        entry.pinned |= workspace.pinned;
        entry.last_used_at = entry.last_used_at.max(workspace.last_used_at);
        entry.has_workspace = true;
    }

    for (key, entries) in &todos {
        let entry = by_key
            .entry(key.clone())
            .or_insert_with(|| GuiTodoProjectCandidate {
                key: key.clone(),
                pinned: false,
                last_used_at: 0,
                has_workspace: false,
                activity_rank: 2,
                todo_count: 0,
            });
        entry.todo_count = entries.len();
    }

    if let Some(agents) = agents {
        for rec in agents.as_array().map(|a| a.as_slice()).unwrap_or(&[]) {
            let rank = match rec.get("state").and_then(|v| v.as_str()) {
                Some("working") => 0u8,
                Some("idle") => 1u8,
                _ => continue,
            };
            let Some(cwd) = rec
                .get("cwd")
                .and_then(|v| v.as_str())
                .filter(|c| !c.is_empty())
            else {
                continue;
            };
            let key = crate::project::detect_from(std::path::Path::new(cwd));
            if key.is_empty() {
                continue;
            }
            let entry = by_key
                .entry(key.clone())
                .or_insert_with(|| GuiTodoProjectCandidate {
                    key,
                    pinned: false,
                    last_used_at: 0,
                    has_workspace: false,
                    activity_rank: rank,
                    todo_count: 0,
                });
            entry.activity_rank = entry.activity_rank.min(rank);
        }
    }

    let mut with_todos: Vec<GuiTodoProjectCandidate> = Vec::new();
    let mut recent: Vec<GuiTodoProjectCandidate> = Vec::new();
    for candidate in by_key.into_values() {
        if candidate.todo_count > 0 {
            with_todos.push(candidate);
        } else if candidate.has_workspace || candidate.activity_rank < 2 {
            // Recent section: workspaces + live agents only (no orphan empty keys).
            recent.push(candidate);
        }
    }
    with_todos.sort_by(gui_todo_project_sort);
    recent.sort_by(gui_todo_project_sort);

    let mut out = Vec::with_capacity(with_todos.len() + recent.len());
    for candidate in with_todos {
        out.push(TodoProjectInfo {
            name: crate::project::display_name(&candidate.key),
            count: candidate.todo_count,
            key: candidate.key,
            section: "withTodos".into(),
        });
    }
    for candidate in recent {
        out.push(TodoProjectInfo {
            name: crate::project::display_name(&candidate.key),
            count: candidate.todo_count,
            key: candidate.key,
            section: "recent".into(),
        });
    }
    out
}

/// 项目选择器候选（本地快路径）：有待办 ∪ 最近 workspace。**不**连 daemon 拉 agent，
/// 供前端首屏瞬时填充下拉；agent 段见 `todos_projects_enriched`。
#[tauri::command]
pub fn todos_projects() -> Vec<TodoProjectInfo> {
    build_todo_project_list(None)
}

/// 项目选择器候选（含活跃 agent）：在本地列表之上合并 daemon agent 快照。
/// 前端在首屏 `todos_projects` 之后后台调用，避免下拉打开前卡在 IPC。
#[tauri::command]
pub async fn todos_projects_enriched() -> Vec<TodoProjectInfo> {
    #[cfg(unix)]
    let agents = crate::client::agents_snapshot_if_running().await;
    #[cfg(not(unix))]
    let agents: Option<serde_json::Value> = None;
    build_todo_project_list(agents.as_ref())
}

/// 打开（或聚焦）项目待办窗口（spec todo-whats-next D9）：经统一宿主路由（全局单窗）。
/// `dir` 为预选项目定位目录（如 agent 的 cwd），后端映射到 git 根 key；None＝前端自选默认。
#[tauri::command]
pub fn open_todos(app: AppHandle, dir: Option<String>) -> Result<(), String> {
    #[cfg(unix)]
    {
        let project = dir
            .filter(|d| !d.trim().is_empty())
            .map(|d| crate::project::detect_from(std::path::Path::new(&d)))
            .filter(|k| !k.is_empty());
        route_open_window(
            app,
            crate::gui_host::WindowKind::Todos,
            false,
            project,
            None,
            None,
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app, dir);
        Err("unsupported".to_string())
    }
}

/// 打开（或聚焦）「新建 Agent 任务」窗口（spec gui-agent-task-launch）：经统一宿主路由
/// （全局单窗）。`project` 为预选项目 key、`todo` 为预选待办 id（待办行入口传入；均可空）。
#[tauri::command]
pub fn open_new_task(
    app: AppHandle,
    project: Option<String>,
    todo: Option<String>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        route_open_window(
            app,
            crate::gui_host::WindowKind::NewTask,
            false,
            project.filter(|p| !p.trim().is_empty()),
            None,
            todo.filter(|t| !t.trim().is_empty()),
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app, project, todo);
        Err("unsupported".to_string())
    }
}

#[tauri::command]
pub fn open_fork_task(app: AppHandle, session: String) -> Result<(), String> {
    let session = session.trim();
    if session.is_empty() {
        return Err("source session is required".into());
    }
    #[cfg(unix)]
    {
        route_open_window(
            app,
            crate::gui_host::WindowKind::ForkTask,
            false,
            None,
            Some(crate::gui_host::InterjectTarget {
                session: session.to_string(),
                agent: None,
                cwd: None,
            }),
            None,
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = app;
        Err("unsupported".into())
    }
}

/// 前端提交的作答内容（按问题顺序，每题一项）。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PopupSubmission {
    #[serde(default)]
    answers: Vec<QuestionAnswer>,
}

#[tauri::command]
pub fn submit_popup(app: AppHandle, submission: PopupSubmission) {
    // GUI Helper 模式：经 IPC 回传 Daemon。
    if let Some(bridge) = app.try_state::<crate::app::GuiBridge>() {
        bridge.send_answer(submission.answers);
        return;
    }
    // 单进程（非 unix 回退）模式：投递本地协调器。
    let result = ChannelResult {
        action: ChannelAction::Send,
        answers: submission.answers,
        source_channel_id: "popup".to_string(),
    };
    if let Some(c) = app.try_state::<Arc<Coordinator>>() {
        c.submit(result);
    }
}

#[tauri::command]
pub fn submit_confirm_action(
    app: AppHandle,
    choice_index: usize,
    comment: Option<String>,
) -> Result<(), String> {
    let bridge = app
        .try_state::<crate::app::GuiBridge>()
        .ok_or_else(|| "confirmation popup requires a daemon bridge".to_string())?;
    bridge.send_confirm_answer(choice_index, comment);
    Ok(())
}

#[tauri::command]
pub fn confirm_popup_ready(app: AppHandle) -> Result<(), String> {
    let bridge = app
        .try_state::<crate::app::GuiBridge>()
        .ok_or_else(|| "confirmation popup requires a daemon bridge".to_string())?;
    bridge.send_confirm_ready();
    Ok(())
}

#[tauri::command]
pub fn cancel_popup(app: AppHandle) {
    if let Some(bridge) = app.try_state::<crate::app::GuiBridge>() {
        bridge.send_cancel();
        return;
    }
    if let Some(c) = app.try_state::<Arc<Coordinator>>() {
        c.submit(ChannelResult::cancel("popup"));
    }
}

// ===== 文件附件：打开 / 预览 / 缩略图 =====

/// 用系统默认程序打开文件（macOS open / Windows start / Linux xdg-open）。
#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    open_with_system(&path)
}

/// 预览附件：macOS 用原生 QLPreviewPanel 展示「全部附件」并定位到 `index`，
/// 面板内方向键即可在附件间切换（与 Finder 一致）；其它平台回退为「打开」当前项。
#[tauri::command]
pub fn preview_attachments(
    app: AppHandle,
    window: tauri::WebviewWindow,
    paths: Vec<String>,
    index: usize,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        // 取【调用方窗口】NSWindow 指针（弹窗或历史窗口）：把预览控制者插入其响应链，
        // 方可经协议控制面板（方向键切换）。回退到 popup 以兼容历史调用方。
        let win_ptr = window
            .ns_window()
            .ok()
            .or_else(|| {
                app.get_webview_window("popup")
                    .and_then(|w| w.ns_window().ok())
            })
            .map(|p| p as usize)
            .unwrap_or(0);
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            crate::macos_quicklook::show(app2, win_ptr, &paths, index);
        });
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, window);
        let path = paths.get(index).ok_or_else(|| {
            crate::i18n::tr(crate::i18n::Lang::current(), "cmd.invalidAttachmentIndex").to_string()
        })?;
        open_with_system(path)
    }
}

/// 关闭当前 QuickLook 预览（点击附件以外区域时调用）。
#[tauri::command]
pub fn close_preview(#[allow(unused_variables)] app: AppHandle) {
    #[cfg(target_os = "macos")]
    {
        let _ = app.run_on_main_thread(|| {
            crate::macos_quicklook::hide();
        });
    }
}

/// 读取本地图片并返回 base64 data URL（供前端缩略图显示）。
#[tauri::command]
pub fn read_image_data_url(path: String) -> Result<String, String> {
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    let bytes = std::fs::read(&path).map_err(|e| {
        crate::i18n::tr(crate::i18n::Lang::current(), "cmd.readFileFailed")
            .replace("{e}", &e.to_string())
    })?;
    let mime = image_mime_from_path(&path);
    Ok(format!("data:{};base64,{}", mime, B64.encode(bytes)))
}

/// 获取文件的系统图标（macOS：NSWorkspace，Finder 同款）并返回 PNG data URL，
/// 供前端把 -f 附件胶囊拖出到其它应用时作为拖拽预览图标。
#[tauri::command]
pub fn file_icon_data_url(app: AppHandle, path: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::mpsc::channel;
        let (tx, rx) = channel();
        app.run_on_main_thread(move || {
            let _ = tx.send(crate::macos_quicklook::file_icon_png_base64(&path));
        })
        .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, path);
        Err(crate::i18n::tr(crate::i18n::Lang::current(), "cmd.fileIconUnsupported").to_string())
    }
}

/// 弹出 -f 附件胶囊的原生右键菜单（Finder 风格）。macOS 专属，其它平台为空操作。
#[tauri::command]
pub fn show_attachment_menu(app: AppHandle, path: String) {
    #[cfg(target_os = "macos")]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || {
            crate::macos_menu::show(app2, path);
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, path);
    }
}

fn open_with_system(path: &str) -> Result<(), String> {
    use std::process::Command;
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = Command::new("open");
        c.arg(path);
        c
    };
    #[cfg(target_os = "windows")]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        c
    };
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    let mut cmd = {
        let mut c = Command::new("xdg-open");
        c.arg(path);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| {
        crate::i18n::tr(crate::i18n::Lang::current(), "cmd.openFailed")
            .replace("{e}", &e.to_string())
    })
}

fn image_mime_from_path(path: &str) -> &'static str {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

pub(crate) fn theme_str(theme: ThemeMode) -> String {
    match theme {
        ThemeMode::System => "system",
        ThemeMode::Light => "light",
        ThemeMode::Dark => "dark",
    }
    .to_string()
}

// ===== 回复历史 =====

/// 历史窗口初始化负载：当前主题 + 语言 + 当前项目（用于默认过滤）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryInit {
    theme: String,
    /// 界面语言（已解析为 `en`/`zh`）。历史窗口据此 `applyLanguage`，使 `main.ts` 无需读配置
    /// （与 `agents_init` 同模式）。
    lang: String,
    /// 当前项目 key（可空）。
    project: String,
    /// 当前项目显示名（basename；可空）。
    project_name: String,
}

#[tauri::command]
pub fn history_init(state: State<AppState>) -> HistoryInit {
    // 主题/语言现读配置（GUI Host 常驻，进程级快照会滞后）；项目仍取本进程状态。
    let config = AppConfig::load_without_secrets();
    HistoryInit {
        theme: theme_str(config.general.theme),
        lang: crate::i18n::Lang::resolve(&config.general.language)
            .code()
            .to_string(),
        project: state.project.clone(),
        project_name: crate::project::display_name(&state.project),
    }
}

/// Agent 状态窗口初始化负载（实验性功能 spec D13）：主题 + 语言（前端据此渲染样式与文案）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsInit {
    theme: String,
    lang: String,
    /// 与弹窗一致的提交快捷键（控制台输入框 ⌘↵ 发送）。
    popup_submit_key: String,
    /// 「新建任务」入口是否可用（spec gui-agent-task-launch G1 同口径）：macOS 且 Terminal.app 存在。
    new_task_supported: bool,
}

#[tauri::command]
pub fn agents_init() -> AgentsInit {
    // 现读配置而非 `AppState.config`（同 `todos_init`，避免常驻宿主的过期快照）。
    let config = AppConfig::load_without_secrets();
    AgentsInit {
        theme: theme_str(config.general.theme),
        lang: crate::i18n::Lang::resolve(&config.general.language)
            .code()
            .to_string(),
        popup_submit_key: config.general.popup_submit_key.as_str().to_string(),
        new_task_supported: crate::integrations::agent_launch::terminal_available(),
    }
}

/// 由 Agent 状态窗口前端在 `agents-updated` 监听就绪后调用，启动到 daemon 的快照订阅
/// （幂等）。延迟到此刻才连 daemon，是为避免 daemon 的首帧立即快照早于前端监听而丢失。
#[tauri::command]
pub fn agents_start_subscription(app: AppHandle) {
    #[cfg(unix)]
    crate::app::start_agents_subscription(app);
    #[cfg(not(unix))]
    let _ = app;
}

/// 把「打开窗口」请求路由到统一 GUI 宿主（spec D3）：宿主在则聚焦/新建（全局单窗），不在则拉起。
/// 失败兜底：在当前（弹窗）进程内直接建窗，保证按钮始终能开窗。整个过程在后台线程进行，
/// 避免阻塞调用方（弹窗 UI 线程）——`host_open` 在宿主冷启动时可能耗时上百毫秒到数秒。
#[cfg(unix)]
fn route_open_window(
    app: AppHandle,
    kind: crate::gui_host::WindowKind,
    all: bool,
    project: Option<String>,
    target: Option<crate::gui_host::InterjectTarget>,
    todo: Option<String>,
) {
    use crate::gui_host::WindowKind;
    std::thread::spawn(move || {
        if crate::gui_host::host_open(kind, all, project.clone(), target.clone(), todo.clone())
            .is_ok()
        {
            return;
        }
        let fallback = app.clone();
        let _ = app.run_on_main_thread(move || {
            let cfg = AppConfig::load_without_secrets();
            // 兜底在弹窗进程内建窗：沿用进程内置顶判定（有弹窗且置顶 → 浮于其上）。
            let pin = crate::app::popup_pin(&fallback, &cfg);
            let _ = match kind {
                WindowKind::Settings => {
                    // `project` 槽位在设置窗口语义下是「初始定位 tab」（同 gui_host::open_window）。
                    crate::app::create_settings_window(&fallback, &cfg, pin, project.as_deref())
                }
                WindowKind::History => crate::app::create_history_window(
                    &fallback,
                    &cfg,
                    all,
                    project.as_deref(),
                    None,
                    pin,
                ),
                WindowKind::Agents => crate::app::create_agents_window(
                    &fallback,
                    &cfg,
                    target.as_ref().map(|t| t.session.as_str()),
                    pin,
                ),
                WindowKind::Interject => match &target {
                    Some(t) => crate::app::create_interject_window(&fallback, &cfg, t, pin),
                    None => Ok(()),
                },
                // `project` 槽位在待办窗口语义下是「预选项目 key」。
                WindowKind::Todos => {
                    crate::app::create_todos_window(&fallback, &cfg, project.as_deref(), pin)
                }
                // `project` = 预选项目 key、`todo` = 预选待办 id（spec gui-agent-task-launch）。
                WindowKind::NewTask => crate::app::create_new_task_window(
                    &fallback,
                    &cfg,
                    project.as_deref(),
                    todo.as_deref(),
                    pin,
                ),
                WindowKind::ForkTask => match &target {
                    Some(t) => {
                        crate::app::create_fork_task_window(&fallback, &cfg, &t.session, pin)
                    }
                    None => Ok(()),
                },
            };
        });
    });
}

fn history_open_target(
    project: &str,
    agent_kind: Option<&str>,
    agent_session_id: Option<&str>,
    mcp_instance_id: Option<&str>,
) -> Option<crate::gui_host::HistoryOpenTarget> {
    fn nonempty(value: Option<&str>) -> Option<&str> {
        value.map(str::trim).filter(|value| !value.is_empty())
    }
    if let (Some(agent_kind), Some(session_id)) = (nonempty(agent_kind), nonempty(agent_session_id))
    {
        return Some(crate::gui_host::HistoryOpenTarget::Agent {
            agent_kind: agent_kind.to_string(),
            session_id: session_id.to_string(),
        });
    }
    nonempty(mcp_instance_id).and_then(|instance_id| {
        let project = project.trim();
        (!project.is_empty()).then(|| crate::gui_host::HistoryOpenTarget::Mcp {
            project: project.to_string(),
            instance_id: instance_id.to_string(),
        })
    })
}

fn effective_popup_history_context(
    app: &AppHandle,
    state: &State<AppState>,
) -> (String, Option<crate::gui_host::HistoryOpenTarget>) {
    #[cfg(unix)]
    if let Some(warm) = app.try_state::<crate::app::WarmPopup>() {
        if let Some(show) = warm.show.lock().ok().and_then(|show| show.clone()) {
            let target = history_open_target(
                &show.project,
                show.agent_kind.as_deref(),
                show.agent_session_id.as_deref(),
                show.mcp_instance_id.as_deref(),
            );
            return (show.project, target);
        }
    }
    let target = history_open_target(
        &state.project,
        state.agent_kind.as_deref(),
        state.agent_session_id.as_deref(),
        state.mcp_instance_id.as_deref(),
    );
    (state.project.clone(), target)
}

#[cfg(unix)]
fn route_open_history_window(
    app: AppHandle,
    project: String,
    target: Option<crate::gui_host::HistoryOpenTarget>,
) {
    std::thread::spawn(move || {
        if crate::gui_host::host_open_history(Some(project.clone()), target.clone()).is_ok() {
            return;
        }
        let fallback = app.clone();
        let _ = app.run_on_main_thread(move || {
            let cfg = AppConfig::load_without_secrets();
            let pin = crate::app::popup_pin(&fallback, &cfg);
            let _ = crate::app::create_history_window(
                &fallback,
                &cfg,
                false,
                Some(project.as_str()),
                target.as_ref(),
                pin,
            );
        });
    });
}

/// Resolve the popup's daemon-validated Agent Window target. Warm helpers keep per-request context
/// in `WarmPopup.show`; cold helpers keep it in `AppState`.
#[cfg(unix)]
fn effective_popup_agent_console_session(
    app: &AppHandle,
    state: &State<AppState>,
) -> Option<String> {
    if let Some(w) = app.try_state::<crate::app::WarmPopup>() {
        if let Some(session_id) = w.show.lock().ok().and_then(|show| {
            show.as_ref()
                .and_then(|payload| payload.agent_console_session_id.clone())
        }) {
            return Some(session_id);
        }
    }
    state.agent_console_session_id.clone()
}

/// Open or focus the Agent Window at the exact session matched by the daemon for this popup.
/// The frontend supplies no session identifier of its own; absence means the shortcut is invalid.
#[tauri::command]
pub fn open_agent_console(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    #[cfg(unix)]
    {
        let session_id = effective_popup_agent_console_session(&app, &state)
            .filter(|session_id| !session_id.trim().is_empty())
            .ok_or_else(|| "no matched agent session".to_string())?;
        route_open_window(
            app,
            crate::gui_host::WindowKind::Agents,
            false,
            None,
            Some(crate::gui_host::InterjectTarget {
                session: session_id,
                agent: None,
                cwd: None,
            }),
            None,
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app, state);
        Err("unsupported".to_string())
    }
}

/// 从弹窗导航栏打开独立历史窗口：路由到统一宿主（全局单窗），默认过滤到弹窗所属项目。
#[tauri::command]
pub fn open_history(app: AppHandle, state: State<AppState>) -> Result<(), String> {
    let (project, target) = effective_popup_history_context(&app, &state);
    #[cfg(unix)]
    {
        route_open_history_window(app, project, target);
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let cfg = AppConfig::load_without_secrets();
        let pin = crate::app::popup_pin(&app, &cfg);
        crate::app::create_history_window(
            &app,
            &cfg,
            false,
            Some(project.as_str()),
            target.as_ref(),
            pin,
        )
        .map_err(|e| e.to_string())
    }
}

/// 读取历史记录：`all` 为 true 时返回全部项目，否则按 `project`（缺省空串）过滤；按时间倒序。
#[tauri::command]
pub fn get_history(project: Option<String>, all: bool) -> Vec<crate::history::HistoryEntry> {
    crate::history::load(project.as_deref(), all)
}

/// 历史中出现过的项目列表（供窗口下拉切换）。
#[tauri::command]
pub fn get_history_projects() -> Vec<crate::history::ProjectInfo> {
    crate::history::projects()
}

/// 当前历史总条数（设置页据此判断是否超额）。
#[tauri::command]
pub fn history_count() -> usize {
    crate::history::count()
}

/// 立即把历史裁剪到 `limit` 条（设置页「立即清理」）。返回裁剪后条数。
#[tauri::command]
pub fn trim_history(limit: u32) -> usize {
    crate::history::trim(limit)
}

/// Delete the exact history entries frozen by the UI before it opened the confirmation dialog.
#[tauri::command]
pub fn delete_history_entries(ids: Vec<String>) -> Result<usize, String> {
    crate::history::delete_ids(&ids).map_err(|error| error.to_string())
}

/// Clear every reply-history entry and report the actual number removed.
#[tauri::command]
pub fn clear_all_history() -> Result<usize, String> {
    crate::history::clear_all().map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySessionTitleRequest {
    token: String,
    agent_kind: String,
    session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistorySessionTitleResult {
    token: String,
    title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct HistoryTitleKey {
    kind: crate::agents::AgentKind,
    session_id: String,
}

#[derive(Debug, Clone)]
struct CachedHistoryTitle {
    title: Option<String>,
    checked_at: Instant,
}

const HISTORY_TITLE_BATCH_MAX: usize = 512;
const HISTORY_TITLE_CACHE_MAX: usize = 512;
const HISTORY_TITLE_HIT_TTL: Duration = Duration::from_secs(5 * 60);
const HISTORY_TITLE_MISS_TTL: Duration = Duration::from_secs(30);

fn history_title_cache() -> &'static std::sync::Mutex<HashMap<HistoryTitleKey, CachedHistoryTitle>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<HashMap<HistoryTitleKey, CachedHistoryTitle>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn resolve_history_session_titles_with(
    requests: Vec<HistorySessionTitleRequest>,
    now: Instant,
    cache: &mut HashMap<HistoryTitleKey, CachedHistoryTitle>,
    mut resolver: impl FnMut(crate::agents::AgentKind, &str) -> Option<String>,
) -> Vec<HistorySessionTitleResult> {
    let mut output = Vec::new();
    let mut seen_tokens = HashSet::new();
    for request in requests.into_iter().take(HISTORY_TITLE_BATCH_MAX) {
        let token = request.token.trim();
        let session_id = request.session_id.trim();
        let Some(kind) = crate::agents::AgentKind::parse(&request.agent_kind) else {
            continue;
        };
        if token.is_empty() || session_id.is_empty() || !seen_tokens.insert(token.to_string()) {
            continue;
        }
        let key = HistoryTitleKey {
            kind,
            session_id: session_id.to_string(),
        };
        let cached = cache.get(&key).filter(|entry| {
            let ttl = if entry.title.is_some() {
                HISTORY_TITLE_HIT_TTL
            } else {
                HISTORY_TITLE_MISS_TTL
            };
            now.saturating_duration_since(entry.checked_at) <= ttl
        });
        let title = match cached {
            Some(entry) => entry.title.clone(),
            None => {
                let resolved = resolver(kind, session_id)
                    .map(|title| title.trim().to_string())
                    .filter(|title| !title.is_empty());
                cache.insert(
                    key,
                    CachedHistoryTitle {
                        title: resolved.clone(),
                        checked_at: now,
                    },
                );
                resolved
            }
        };
        if let Some(title) = title {
            output.push(HistorySessionTitleResult {
                token: token.to_string(),
                title,
            });
        }
    }
    while cache.len() > HISTORY_TITLE_CACHE_MAX {
        let Some(oldest) = cache
            .iter()
            .min_by_key(|(_, entry)| entry.checked_at)
            .map(|(key, _)| key.clone())
        else {
            break;
        };
        cache.remove(&oldest);
    }
    output
}

/// Resolve exact native Agent session titles in one best-effort cached batch.
#[tauri::command]
pub async fn resolve_history_session_titles(
    requests: Vec<HistorySessionTitleRequest>,
) -> Vec<HistorySessionTitleResult> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut cache = history_title_cache().lock().unwrap();
        resolve_history_session_titles_with(
            requests,
            Instant::now(),
            &mut cache,
            crate::agents::title::resolve_title,
        )
    })
    .await
    .unwrap_or_default()
}

// ===== 设置页命令 =====

/// Whether each channel secret is currently configured (keychain or plaintext fallback). Drives
/// the settings page "Saved" placeholder without ever exposing the secret value.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretsPresent {
    dingding_secret: bool,
    feishu_secret: bool,
    telegram_token: bool,
    slack_bot_token: bool,
    slack_app_token: bool,
}

/// Settings payload: the config with secrets blanked, plus per-secret presence flags.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPayload {
    config: AppConfig,
    secrets_present: SecretsPresent,
}

/// Per-secret edit intent sent by the settings page on save. The secret value never round-trips
/// through the config object; it is carried only here (and only for `set`).
#[derive(Deserialize, Default)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SecretAction {
    /// Keep the stored secret as-is (the user did not touch the field).
    #[default]
    Unchanged,
    /// Replace the stored secret with `value`.
    Set { value: String },
    /// Delete the stored secret from the keychain.
    Clear,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SecretActions {
    dingding_secret: SecretAction,
    feishu_secret: SecretAction,
    telegram_token: SecretAction,
    slack_bot_token: SecretAction,
    slack_app_token: SecretAction,
}

#[tauri::command]
pub fn get_settings() -> SettingsPayload {
    let mut config = AppConfig::load();
    // Presence is derived from the resolved value (keychain first, plaintext fallback).
    let secrets_present = SecretsPresent {
        dingding_secret: !config.channels.dingding.client_secret.is_empty(),
        feishu_secret: !config.channels.feishu.app_secret.is_empty(),
        telegram_token: !config.channels.telegram.bot_token.is_empty(),
        slack_bot_token: !config.channels.slack.bot_token.is_empty(),
        slack_app_token: !config.channels.slack.app_token.is_empty(),
    };
    // Never let resolved secrets reach the UI; the page shows a fixed-length placeholder instead.
    config.channels.dingding.client_secret.clear();
    config.channels.feishu.app_secret.clear();
    config.channels.telegram.bot_token.clear();
    config.channels.slack.bot_token.clear();
    config.channels.slack.app_token.clear();
    SettingsPayload {
        config,
        secrets_present,
    }
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    mut config: AppConfig,
    secret_actions: SecretActions,
) -> Result<(), String> {
    if config.agent_tasks.enabled {
        config.general.daemon_lifecycle = crate::config::DaemonLifecycleMode::KeepAlive;
        #[cfg(unix)]
        crate::integrations::login_item::sync_daemon(true).map_err(|e| e.to_string())?;
    }
    // Secrets are governed solely by the explicit actions (the incoming config carries blank
    // placeholders). unchanged → leave the field empty so save() won't touch the keychain;
    // set → store it via save(); clear → delete from the keychain now.
    apply_secret_action(
        &mut config.channels.dingding.client_secret,
        crate::secrets::ACCOUNT_DINGTALK_SECRET,
        secret_actions.dingding_secret,
    );
    apply_secret_action(
        &mut config.channels.feishu.app_secret,
        crate::secrets::ACCOUNT_FEISHU_SECRET,
        secret_actions.feishu_secret,
    );
    apply_secret_action(
        &mut config.channels.telegram.bot_token,
        crate::secrets::ACCOUNT_TELEGRAM_TOKEN,
        secret_actions.telegram_token,
    );
    apply_secret_action(
        &mut config.channels.slack.bot_token,
        crate::secrets::ACCOUNT_SLACK_BOT_TOKEN,
        secret_actions.slack_bot_token,
    );
    apply_secret_action(
        &mut config.channels.slack.app_token,
        crate::secrets::ACCOUNT_SLACK_APP_TOKEN,
        secret_actions.slack_app_token,
    );
    config.save().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    if config.agent_tasks.enabled {
        crate::client::ensure_running()
            .await
            .map_err(|e| e.to_string())?;
    }
    // 广播 general 配置，令同进程内已打开的弹窗实时生效（如语音语言/快捷键）。
    let _ = app.emit("settings-updated", &config.general);
    // 界面语言可能变化：实时更新已打开窗口的原生标题（弹窗标题在 macOS 多隐藏，settings 可见）。
    let lang = crate::i18n::Lang::resolve(&config.general.language);
    if let Some(w) = app.get_webview_window("settings") {
        let _ = w.set_title(crate::i18n::tr(lang, "title.settings"));
    }
    if let Some(w) = app.get_webview_window("popup") {
        let _ = w.set_title(crate::i18n::tr(lang, "title.popup"));
    }
    Ok(())
}

/// 权限授权管理面板（spec codex-permission-remember §6.3）：设置前端唯一入口，全部操作
/// 经 daemon 完成（读摘要 / 展开详情 / 重置），设置进程不直接读写 rule store 文件。
#[tauri::command]
pub async fn permission_rules_panel(
    op: crate::ipc::PermissionRulesOp,
) -> Result<crate::ipc::PermissionRulesResult, String> {
    #[cfg(unix)]
    {
        crate::client::permission_rules_op(op).await
    }
    #[cfg(not(unix))]
    {
        let _ = op;
        Err("unsupported platform".to_string())
    }
}

#[tauri::command]
pub fn agent_task_workspaces(refresh: Option<bool>) -> Vec<crate::agents::workspaces::Workspace> {
    if refresh.unwrap_or(false) {
        let _ = crate::agents::workspaces::refresh();
        crate::agents::workspaces::list()
    } else {
        crate::agents::workspaces::list()
    }
}

#[tauri::command]
pub fn agent_task_workspace_add(
    path: String,
) -> Result<crate::agents::workspaces::Workspace, String> {
    crate::agents::workspaces::add(std::path::Path::new(&path), false)
}

/// Open the native system directory picker used by the Agent-task workspace manager.
#[tauri::command]
pub fn agent_task_workspace_pick(app: AppHandle) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    {
        use std::sync::mpsc::channel;
        let (tx, rx) = channel();
        app.run_on_main_thread(move || {
            let _ = tx.send(crate::macos_menu::choose_directory());
        })
        .map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())?
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("Agent task workspace picker is only supported on macOS".to_string())
    }
}

#[tauri::command]
pub fn agent_task_workspace_pin(path: String, pinned: bool) -> Result<(), String> {
    crate::agents::workspaces::set_pinned(&path, pinned)
}

#[tauri::command]
pub fn agent_task_workspace_hide(path: String, hidden: bool) -> Result<(), String> {
    crate::agents::workspaces::set_hidden(&path, hidden)
}

#[tauri::command]
pub fn agent_task_workspace_forget(path: String) -> Result<(), String> {
    crate::agents::workspaces::forget(&path)
}

#[tauri::command]
pub async fn agent_task_readiness(
) -> Result<Vec<crate::integrations::agent_launch::AgentReadiness>, String> {
    tokio::task::spawn_blocking(crate::integrations::agent_launch::all_readiness)
        .await
        .map_err(|e| e.to_string())
}

/// Open a harmless Terminal.app self-check. It never resolves or starts an Agent binary.
#[tauri::command]
pub fn agent_task_test_terminal() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let script = r#"tell application "Terminal"
activate
do script "printf '\\nAskHuman Terminal test succeeded.\\n'"
end tell"#;
        let status = std::process::Command::new("/usr/bin/osascript")
            .args(["-e", script])
            .status()
            .map_err(|e| e.to_string())?;
        status
            .success()
            .then_some(())
            .ok_or_else(|| "Terminal.app rejected the test".to_string())
    }
    #[cfg(not(target_os = "macos"))]
    Err("Terminal.app test is only available on macOS".to_string())
}

// ===== 「新建 Agent 任务」窗口（spec gui-agent-task-launch）=====

/// 新建任务窗口初始化负载。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTaskInit {
    theme: String,
    lang: String,
    /// 与弹窗一致的提交快捷键（⌘↵ 启动任务）。
    popup_submit_key: String,
    /// `agentTasks.permissionPrompt`：`ask` / `agent-default` / `yolo`（G6）。
    permission_prompt: String,
}

#[tauri::command]
pub fn new_task_init() -> NewTaskInit {
    // 现读配置（同 `todos_init`，避免常驻宿主的过期快照）。
    let config = AppConfig::load_without_secrets();
    let permission_prompt = match config.agent_tasks.permission_prompt {
        crate::config::AgentTaskPermission::Ask => "ask",
        crate::config::AgentTaskPermission::AgentDefault => "agent-default",
        crate::config::AgentTaskPermission::Yolo => "yolo",
    };
    NewTaskInit {
        theme: theme_str(config.general.theme),
        lang: crate::i18n::Lang::resolve(&config.general.language)
            .code()
            .to_string(),
        popup_submit_key: config.general.popup_submit_key.as_str().to_string(),
        permission_prompt: permission_prompt.to_string(),
    }
}

/// 新建任务窗口的项目下拉候选。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewTaskProject {
    /// workspace 路径（canonical cwd，保留子目录/worktree 语义）或待办项目 git 根。
    pub path: String,
    /// 显示名（basename）。
    pub label: String,
    /// 置顶 workspace（排序已体现；前端加 ★ 前缀）。
    pub pinned: bool,
    /// `workspace`（最近 workspace 索引）或 `todos`（仅存在于待办存储）。
    pub source: String,
}

/// workspace 索引（refresh/list 已按 pinned → last_used 排序）在前，仅存在于待办存储的
/// git 根在后（spec §4）；按路径去重、过滤 hidden 与不存在目录。
fn build_new_task_projects(
    workspaces: Vec<crate::agents::workspaces::Workspace>,
    todo_projects: Vec<String>,
) -> Vec<NewTaskProject> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for workspace in workspaces {
        if workspace.hidden || !std::path::Path::new(&workspace.path).is_dir() {
            continue;
        }
        if seen.insert(workspace.path.clone()) {
            out.push(NewTaskProject {
                label: workspace.label,
                pinned: workspace.pinned,
                source: "workspace".into(),
                path: workspace.path,
            });
        }
    }
    let mut todo_keys: Vec<String> = todo_projects
        .into_iter()
        .filter(|key| std::path::Path::new(key).is_dir() && !seen.contains(key))
        .collect();
    todo_keys.sort();
    todo_keys.dedup();
    for key in todo_keys {
        out.push(NewTaskProject {
            label: crate::project::display_name(&key),
            pinned: false,
            source: "todos".into(),
            path: key,
        });
    }
    out
}

/// 当前有待办的项目 key（git 根）。
fn todo_project_keys() -> Vec<String> {
    crate::todos::all()
        .into_iter()
        .filter(|(_, entries)| !entries.is_empty())
        .map(|(key, _)| key)
        .collect()
}

/// 项目候选（本地快路径）：workspace 索引 + 待办项目，供首屏即时填充。
#[tauri::command]
pub fn new_task_projects() -> Vec<NewTaskProject> {
    build_new_task_projects(crate::agents::workspaces::list(), todo_project_keys())
}

/// 项目候选（含四家有界冷扫描合并，与 IM `/new` 的 `workspaces::refresh()` 同源）。
/// 前端首屏后后台调用，完成后无感刷新下拉。
#[tauri::command]
pub async fn new_task_projects_refreshed() -> Vec<NewTaskProject> {
    let workspaces = tokio::task::spawn_blocking(crate::agents::workspaces::refresh)
        .await
        .unwrap_or_default();
    build_new_task_projects(workspaces, todo_project_keys())
}

/// 目录 → 项目 key（git 根，回退自身）。新建任务窗口按所选 workspace 读取所属项目待办。
#[tauri::command]
pub fn project_key_of(dir: String) -> String {
    crate::project::detect_from(std::path::Path::new(&dir))
}

/// 启动新任务（spec gui-agent-task-launch §2.5）：复用 IM 的 LaunchRecord + Terminal.app 链路。
/// `task` 由前端拼装（选待办时 = 待办原文快照 + 空行 + 补充）；`todo_project`/`todo_id` 非空时
/// Terminal 成功打开后 best-effort 出队（G7）。成功后 best-effort 把活跃槽切到 popup（G11）。
#[tauri::command]
pub async fn new_task_launch(
    workspace: String,
    kind: String,
    permission: String,
    task: String,
    todo_project: Option<String>,
    todo_id: Option<String>,
    todo_attachments: Option<Vec<crate::todo_attachments::TodoAttachmentSnapshot>>,
) -> Result<(), String> {
    let kind = crate::agents::AgentKind::parse(&kind).ok_or("unknown agent kind")?;
    let permission = match permission.as_str() {
        "agent-default" => crate::integrations::agent_launch::LaunchPermission::AgentDefault,
        "yolo" => crate::integrations::agent_launch::LaunchPermission::Yolo,
        _ => return Err("permission choice is required".to_string()),
    };
    if task.contains('\0') {
        return Err("Task must not contain NUL characters".to_string());
    }
    // create_record 内部含 login-shell readiness 复检（≤2s 阻塞探测），放 blocking 线程。
    tokio::task::spawn_blocking(move || {
        let delivery_request_id = uuid::Uuid::new_v4().to_string();
        let delivery =
            if let (Some(project), Some(id)) = (todo_project.as_deref(), todo_id.as_deref()) {
                crate::todos::prepare_delivery_consistent(
                    project,
                    id,
                    &todo_attachments.unwrap_or_default(),
                    &delivery_request_id,
                )
            } else {
                crate::todo_attachments::TodoDelivery::default()
            };
        let source = crate::integrations::agent_launch::LaunchSource {
            channel: "gui".to_string(),
            target: String::new(),
        };
        let record = match crate::integrations::agent_launch::create_record_with_files(
            source,
            std::path::Path::new(&workspace),
            kind,
            permission,
            &task,
            &delivery.files,
            &delivery.warnings,
        ) {
            Ok(record) => record,
            Err(error) => {
                crate::todo_attachments::cleanup_delivery(&delivery_request_id);
                return Err(format!("{error:#}"));
            }
        };
        if let Err(error) = crate::integrations::agent_launch::open_terminal(&record) {
            crate::todo_attachments::cleanup_delivery(&delivery_request_id);
            return Err(format!("{error:#}"));
        }
        // 成功后才出队（G7）；并发删除不报错（take 为 best-effort）。
        if let (Some(project), Some(id)) = (todo_project.as_deref(), todo_id.as_deref()) {
            if !project.is_empty() && !id.is_empty() {
                let _ = crate::todos::take(project, std::slice::from_ref(&id.to_string()));
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    // 活跃槽切 popup（G11）：人在电脑旁，新 Agent 的提问默认弹窗。即发即走，不影响启动结果。
    #[cfg(unix)]
    tokio::spawn(crate::client::activate_popup_slot());
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkTaskSource {
    session_id: String,
    seq: u64,
    kind: String,
    title: String,
    cwd: String,
    state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    forked_from_session_id: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkTaskInit {
    theme: String,
    lang: String,
    popup_submit_key: String,
    permission_prompt: String,
    source: ForkTaskSource,
}

fn fork_task_source(
    snapshot: &serde_json::Value,
    session_id: &str,
    active_only: bool,
) -> Result<ForkTaskSource, String> {
    let record = snapshot
        .as_array()
        .and_then(|items| {
            items.iter().find(|record| {
                record.get("sessionId").and_then(serde_json::Value::as_str) == Some(session_id)
            })
        })
        .ok_or_else(|| "source session is no longer tracked".to_string())?;
    let state = record
        .get("state")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if active_only && !matches!(state, "working" | "idle") {
        return Err("only working or idle sessions can be selected".into());
    }
    let kind = record
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .and_then(crate::agents::AgentKind::parse)
        .ok_or_else(|| "unknown source Agent".to_string())?;
    let cwd = record
        .get("cwd")
        .and_then(serde_json::Value::as_str)
        .filter(|cwd| std::path::Path::new(cwd).is_dir())
        .ok_or_else(|| "source workspace is unavailable".to_string())?;
    if crate::agents::transcript_full::transcript_mtime(kind, session_id).is_none() {
        return Err("source session transcript is unavailable".into());
    }
    Ok(ForkTaskSource {
        session_id: session_id.to_string(),
        seq: record
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default(),
        kind: kind.as_str().to_string(),
        title: record
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        cwd: cwd.to_string(),
        state: state.to_string(),
        forked_from_session_id: record
            .get("forkedFromSessionId")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
    })
}

#[tauri::command]
pub async fn fork_task_init(session: String) -> Result<ForkTaskInit, String> {
    if !crate::integrations::agent_launch::terminal_available() {
        return Err("Terminal.app is unavailable".into());
    }
    #[cfg(unix)]
    let snapshot = crate::client::agents_snapshot_if_running()
        .await
        .ok_or_else(|| "Agent daemon is unavailable".to_string())?;
    #[cfg(not(unix))]
    let snapshot = serde_json::Value::Array(Vec::new());
    let source = fork_task_source(&snapshot, session.trim(), true)?;
    let kind = crate::agents::AgentKind::parse(&source.kind).ok_or("unknown source Agent")?;
    let readiness = tokio::task::spawn_blocking(move || {
        crate::integrations::agent_launch::fork_readiness(kind)
    })
    .await
    .map_err(|error| error.to_string())?;
    if !readiness.ready {
        return Err(readiness.diagnostics.join("; "));
    }
    let config = AppConfig::load_without_secrets();
    let permission_prompt = match config.agent_tasks.permission_prompt {
        crate::config::AgentTaskPermission::Ask => "ask",
        crate::config::AgentTaskPermission::AgentDefault => "agent-default",
        crate::config::AgentTaskPermission::Yolo => "yolo",
    };
    Ok(ForkTaskInit {
        theme: theme_str(config.general.theme),
        lang: crate::i18n::Lang::resolve(&config.general.language)
            .code()
            .to_string(),
        popup_submit_key: config.general.popup_submit_key.as_str().to_string(),
        permission_prompt: permission_prompt.into(),
        source,
    })
}

#[tauri::command]
pub async fn fork_task_launch(
    session: String,
    permission: String,
    task: String,
) -> Result<(), String> {
    let permission = match permission.as_str() {
        "agent-default" => crate::integrations::agent_launch::LaunchPermission::AgentDefault,
        "yolo" => crate::integrations::agent_launch::LaunchPermission::Yolo,
        _ => return Err("permission choice is required".into()),
    };
    if task.contains('\0') {
        return Err("Fork instruction must not contain NUL characters".into());
    }
    let session = session.trim().to_string();
    #[cfg(unix)]
    let snapshot = crate::client::agents_snapshot_if_running()
        .await
        .ok_or_else(|| "Agent daemon is unavailable".to_string())?;
    #[cfg(not(unix))]
    let snapshot = serde_json::Value::Array(Vec::new());
    // Re-resolve kind/cwd from the daemon's locked source identity. The source may have ended
    // since the window opened, but renderer-provided adapter/path values are never trusted.
    let locked_source = fork_task_source(&snapshot, &session, false)?;
    let kind = crate::agents::AgentKind::parse(&locked_source.kind)
        .ok_or_else(|| "unknown source Agent".to_string())?;
    let cwd = locked_source.cwd;
    let source = crate::integrations::agent_launch::LaunchSource {
        channel: "gui".into(),
        target: String::new(),
    };
    let record = tokio::task::spawn_blocking(move || {
        crate::integrations::agent_launch::create_fork_record(
            source,
            std::path::Path::new(&cwd),
            kind,
            permission,
            &session,
            &task,
        )
        .map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())??;
    #[cfg(unix)]
    crate::client::register_launch(&record).await;
    let launch_id = record.id.clone();
    let result = tokio::task::spawn_blocking(move || {
        crate::integrations::agent_launch::open_terminal(&record)
            .map_err(|error| format!("{error:#}"))
    })
    .await
    .map_err(|error| error.to_string())?;
    if result.is_err() {
        #[cfg(unix)]
        crate::client::cancel_launch(launch_id).await;
    }
    result
}

/// Apply one secret's edit intent to the in-memory config field before persisting.
fn apply_secret_action(field: &mut String, account: &str, action: SecretAction) {
    match action {
        SecretAction::Unchanged => field.clear(),
        SecretAction::Set { value } => *field = value,
        SecretAction::Clear => {
            let _ = crate::secrets::delete(account);
            field.clear();
        }
    }
}

/// Resolve the secret to use for a test/detect call. The settings form sends an empty secret when
/// the user kept the "Saved" placeholder; fall back to the effective secret (keychain or plaintext
/// config fallback) so they need not retype it. A non-empty `provided` always wins.
fn fallback_secret(provided: &str, pick: impl FnOnce(&AppConfig) -> String) -> String {
    if !provided.trim().is_empty() {
        return provided.to_string();
    }
    pick(&AppConfig::load())
}

/// 返回参考提示词正文：`variant = "mcp"` → MCP 版；其余（含缺省）→ CLI 版。
/// 供手动集成卡按 CLI/MCP 切换展示。
#[tauri::command]
pub fn get_prompt(variant: Option<String>) -> String {
    match variant.as_deref() {
        Some("mcp") => crate::prompts::mcp_reference(),
        _ => crate::prompts::cli_reference(),
    }
}

/// Default collaboration-style paragraphs for the custom editor (aligned + autonomous).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationStyleDefaults {
    aligned: String,
    autonomous: String,
}

#[tauri::command]
pub fn collaboration_style_defaults() -> CollaborationStyleDefaults {
    CollaborationStyleDefaults {
        aligned: crate::prompts::default_aligned_collaboration_text().to_string(),
        autonomous: crate::prompts::default_autonomous_collaboration_text().to_string(),
    }
}

/// After collaboration style changes: rewrite rules/skill for every agent whose mode is not None.
#[tauri::command]
pub fn collaboration_style_apply_integrations() -> Result<(), String> {
    use crate::integrations::agent_mode::{self, Mode};
    use crate::integrations::agent_rules::AgentTarget;
    for target in [
        AgentTarget::Cursor,
        AgentTarget::ClaudeCode,
        AgentTarget::Codex,
        AgentTarget::Grok,
    ] {
        let mode = agent_mode::current(target);
        if mode == Mode::None {
            continue;
        }
        agent_mode::update(target).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// 设置页「弹出测试窗口」：以独立子进程跑一个多问题示例，
/// 完全复用真实弹窗流程并读取已保存的配置（含出现动画），便于快速预览效果。
#[tauri::command]
pub fn open_test_popup() -> Result<(), String> {
    use std::process::{Command, Stdio};
    let lang = crate::i18n::Lang::current();
    let exe = std::env::current_exe()
        .map_err(|e| crate::i18n::tr(lang, "cmd.locateExeFailed").replace("{e}", &e.to_string()))?;
    Command::new(exe)
        .args([
            crate::i18n::tr(lang, "test.message"),
            "-q",
            crate::i18n::tr(lang, "test.questionAppearance"),
            "-o!",
            crate::i18n::tr(lang, "test.optionGood"),
            "-o",
            crate::i18n::tr(lang, "test.optionAdjust"),
            "-q",
            crate::i18n::tr(lang, "test.questionAnimation"),
            "-o!",
            crate::i18n::tr(lang, "test.optionSmooth"),
            "-o",
            crate::i18n::tr(lang, "test.optionLaggy"),
            "-q",
            crate::i18n::tr(lang, "test.questionSuggestions"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| crate::i18n::tr(lang, "cmd.testPopupFailed").replace("{e}", &e.to_string()))?;
    Ok(())
}

/// Popup sound platform support for settings UI rendering.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PopupSoundSupport {
    /// `"named"` (macOS with `names`), `"toggle"` (Linux), or `"none"` (hidden).
    kind: String,
    /// Optional sound names, only non-empty for `"named"`.
    names: Vec<String>,
}

#[tauri::command]
pub fn popup_sound_support() -> PopupSoundSupport {
    PopupSoundSupport {
        kind: crate::sound::support().to_string(),
        names: crate::sound::names(),
    }
}

/// Settings preview action. Empty string does not play anything.
#[tauri::command]
pub fn play_popup_sound(name: String) {
    crate::sound::play(&name);
}

/// 实时应用主题到已打开的窗口（system→跟随系统）。
#[tauri::command]
pub fn set_theme(app: AppHandle, theme: String) {
    apply_theme_to_windows(&app, &theme);
}

/// 从弹窗导航栏切换主题：写入配置并实时应用到所有窗口。
#[tauri::command]
pub fn update_theme(app: AppHandle, theme: String) -> Result<(), String> {
    // Only the theme changes; load without resolving secrets so save() neither reads nor rewrites
    // the keychain (blank secret fields are left as-is by save()).
    let mut cfg = AppConfig::load_without_secrets();
    cfg.general.theme = match theme.as_str() {
        "light" => ThemeMode::Light,
        "dark" => ThemeMode::Dark,
        _ => ThemeMode::System,
    };
    cfg.save().map_err(|e| e.to_string())?;
    apply_theme_to_windows(&app, &theme);
    Ok(())
}

/// Apply native appearance to every WebView window and refresh Solid's native safety backing.
pub(crate) fn apply_theme_to_windows(app: &AppHandle, theme: &str) {
    let t = match theme {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        _ => None,
    };
    for (_label, window) in app.webview_windows() {
        let _ = window.set_theme(t);
    }
    let config = AppConfig::load_without_secrets();
    if matches!(config.general.window_effect, WindowEffect::Solid) {
        crate::app::refresh_solid_window_backgrounds(
            app,
            crate::app::background_for_theme_name(theme),
        );
    }
}

/// 从弹窗导航栏打开设置窗口（同进程内创建，不影响弹窗等待）。
/// `tab` 可选：打开后定位到指定 tab（如 R6 引导跳「渠道」）。
#[tauri::command]
pub fn open_settings(app: AppHandle, tab: Option<String>) -> Result<(), String> {
    #[cfg(unix)]
    {
        // 路由到统一宿主（全局单窗）；宿主不可用时回退到本进程内建窗。
        route_open_window(
            app,
            crate::gui_host::WindowKind::Settings,
            false,
            tab,
            None,
            None,
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // Settings window only needs general (theme) to build; the page fetches secret presence via
        // get_settings() separately. Skip keychain here.
        let cfg = AppConfig::load_without_secrets();
        let pin = crate::app::popup_pin(&app, &cfg);
        crate::app::create_settings_window(&app, &cfg, pin, tab.as_deref())
            .map_err(|e| e.to_string())
    }
}

// ===== 弹窗一次性引导（R6）=====

/// 弹窗是否应显示「配置 IM 渠道」一次性引导：未被关闭过，且当前没有任何 IM 渠道启用。
#[tauri::command]
pub fn popup_im_tip_visible() -> bool {
    if crate::uistate::load().im_tip_dismissed {
        return false;
    }
    let ch = AppConfig::load_without_secrets().channels;
    !(ch.telegram.enabled || ch.dingding.enabled || ch.feishu.enabled || ch.slack.enabled)
}

/// 永久关闭「配置 IM 渠道」引导（点 ✕ 或点「打开设置」时调用）。
#[tauri::command]
pub fn popup_im_tip_dismiss() {
    let mut s = crate::uistate::load();
    s.im_tip_dismissed = true;
    crate::uistate::save(&s);
}

/// 实时切换窗口材质（纯色/模糊/玻璃）到本进程**全部**已打开 WebView 窗口（含 history/agents 等）。
/// 仅 macOS 真正切换材质。持久化由前端 `save_settings` 负责；此命令只负责即时生效。
#[tauri::command]
pub fn apply_window_effect(app: AppHandle, effect: WindowEffect) {
    crate::app::apply_window_effect_to_all(&app, effect);
}

/// 渠道健康快照（R7）：向 daemon 查询各渠道最近未恢复的故障，设置页渠道 tab 据此显示错误横幅。
/// daemon 未运行（或非 Unix 无 daemon）→ 空列表。
#[tauri::command]
pub async fn channel_health() -> Vec<crate::ipc::ChannelIssueInfo> {
    #[cfg(unix)]
    {
        crate::client::request_status()
            .await
            .map(|s| s.channel_issues)
            .unwrap_or_default()
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

// ===== 语音输入（macOS 26 SpeechAnalyzer，离线，经 Swift 桥） =====

/// 开始语音输入：识别结果经 `speech-committed` / `speech-volatile` 等事件回传。
/// `locale` 为 BCP-47（如 zh-CN），空串=跟随系统。仅 macOS 实现；其它平台为空操作。
#[tauri::command]
pub fn start_speech(
    #[allow(unused_variables)] app: AppHandle,
    #[allow(unused_variables)] locale: Option<String>,
) {
    #[cfg(target_os = "macos")]
    crate::speech::start(app, locale.as_deref().unwrap_or(""));
}

/// 停止语音输入。仅 macOS 实现；其它平台为空操作。
#[tauri::command]
pub fn stop_speech(#[allow(unused_variables)] app: AppHandle) {
    #[cfg(target_os = "macos")]
    crate::speech::stop();
}

/// 听写中途移动光标时：固定已写文本并重启识别会话。仅 macOS。
#[tauri::command]
pub fn flush_speech(#[allow(unused_variables)] app: AppHandle) {
    #[cfg(target_os = "macos")]
    crate::speech::flush();
}

/// 语音输入是否可用（macOS 26+）。非 macOS 或低版本返回 false。
#[tauri::command]
pub fn speech_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        return crate::speech::is_available();
    }
    #[allow(unreachable_code)]
    false
}

// ===== Cursor Hook =====

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HookStatus {
    installed: bool,
    /// 已安装但脚本与最新内置版本不一致 → 需更新。
    outdated: bool,
    hooks_json_exists: bool,
    supported: bool,
}

#[tauri::command]
pub fn cursor_hook_status() -> HookStatus {
    HookStatus {
        installed: cursor_hook::is_installed(),
        outdated: cursor_hook::needs_update(),
        hooks_json_exists: cursor_hook::hooks_json_exists(),
        supported: cursor_hook::supported(),
    }
}

#[tauri::command]
pub fn cursor_hook_install() -> Result<String, String> {
    cursor_hook::install().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cursor_hook_update() -> Result<String, String> {
    cursor_hook::update().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cursor_hook_uninstall() -> Result<String, String> {
    cursor_hook::uninstall().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn cursor_hook_reveal() {
    cursor_hook::reveal();
}

// ===== Claude Code Hook（PreToolUse 超时延长） =====

use crate::integrations::claude_hook;

/// Claude Code Hook 安装状态（与 Cursor Hook 对称，驱动设置页徽标与按钮）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeHookStatus {
    installed: bool,
    /// 已安装但脚本与最新内置版本不一致 → 需更新。
    outdated: bool,
    settings_exists: bool,
    supported: bool,
}

#[tauri::command]
pub fn claude_hook_status() -> ClaudeHookStatus {
    ClaudeHookStatus {
        installed: claude_hook::is_installed(),
        outdated: claude_hook::needs_update(),
        settings_exists: claude_hook::settings_exists(),
        supported: claude_hook::supported(),
    }
}

#[tauri::command]
pub fn claude_hook_install() -> Result<String, String> {
    claude_hook::install().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn claude_hook_update() -> Result<String, String> {
    claude_hook::update().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn claude_hook_uninstall() -> Result<String, String> {
    claude_hook::uninstall().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn claude_hook_reveal() {
    claude_hook::reveal();
}

// ===== Agent 全局规则（Cursor / Claude Code / Codex） =====

use crate::integrations::agent_rules::{self, AgentTarget};

/// Rules 安装状态（驱动设置页 Agent 分组的徽标与按钮）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleStatus {
    installed: bool,
    /// 已安装但提示词正文与最新内置版本不一致 → 需更新。
    outdated: bool,
    /// 展示用文件路径（home 折叠为 ~）。
    path: String,
    supported: bool,
}

fn parse_agent(agent: &str) -> Result<AgentTarget, String> {
    AgentTarget::parse(agent).ok_or_else(|| {
        crate::i18n::tr(crate::i18n::Lang::current(), "cmd.unknownAgent").to_string()
    })
}

#[tauri::command]
pub fn agent_rule_status(agent: String) -> Result<RuleStatus, String> {
    let a = parse_agent(&agent)?;
    Ok(RuleStatus {
        installed: agent_rules::is_installed(a),
        outdated: agent_rules::needs_update(a),
        path: agent_rules::display_path(a),
        supported: agent_rules::supported(a),
    })
}

#[tauri::command]
pub fn agent_rule_install(agent: String) -> Result<String, String> {
    let a = parse_agent(&agent)?;
    agent_rules::install(a).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn agent_rule_update(agent: String) -> Result<String, String> {
    let a = parse_agent(&agent)?;
    agent_rules::update(a).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn agent_rule_uninstall(agent: String) -> Result<String, String> {
    let a = parse_agent(&agent)?;
    agent_rules::uninstall(a).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn agent_rule_reveal(agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    agent_rules::reveal(a);
    Ok(())
}

#[tauri::command]
pub fn agent_rule_open(agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    agent_rules::open(a);
    Ok(())
}

// ===== Agent 三态模式（CLI | MCP | 未集成） =====

use crate::integrations::{
    agent_ask_question, agent_context_recovery, agent_mode, agent_permission, agent_stop,
    mcp_config,
};

/// 某家 Agent 的模式聚合状态（驱动设置页三态分段控件 + 产物清单）。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModeStatus {
    /// 当前模式："none" | "cli" | "mcp"。
    mode: String,
    /// 当前模式下是否有产物过期 / 缺失（= 下面三个 per-artifact 标志的或）。
    needs_update: bool,
    /// 当前模式下 Rule / 超时 Hook / MCP 配置各自是否过期或缺失（驱动单项更新按钮 + 概览统计）。
    rule_needs_update: bool,
    hook_needs_update: bool,
    mcp_needs_update: bool,
    /// Rule 文件展示路径（home 折叠为 ~）。
    rule_path: String,
    rule_installed: bool,
    /// 该 Agent 是否有「超时 Hook」概念（Codex 没有）。
    timeout_hook_supported: bool,
    timeout_hook_installed: bool,
    timeout_hook_needs_update: bool,
    recovery_hook_installed: bool,
    /// PermissionRequest capability state; kept separate from the timeout hook.
    permission: agent_permission::PermissionStatus,
    permission_needs_update: bool,
    /// Stop confirmation preference; activation is integration-mode gated while lifecycle tracking
    /// remains independent.
    stop: agent_stop::StopStatus,
    /// Claude question takeover preference (spec claude-ask-user-question D3).
    ask_question: agent_ask_question::AskQuestionStatus,
    /// MCP 配置文件展示路径。
    mcp_config_path: String,
    mcp_config_installed: bool,
}

#[tauri::command]
pub fn agent_mode_status(agent: String) -> Result<AgentModeStatus, String> {
    let a = parse_agent(&agent)?;
    let stop_kind =
        crate::agents::AgentKind::parse(&agent).ok_or_else(|| "unknown agent".to_string())?;
    let updates = agent_mode::artifact_updates(a);
    let mode = agent_mode::current(a);
    let permission = agent_permission::status(a);
    let recovery = agent_context_recovery::status(a, mode);
    let permission_needs_update = permission.needs_update;
    Ok(AgentModeStatus {
        mode: mode.as_str().to_string(),
        needs_update: updates.rule || updates.hook || updates.mcp,
        rule_needs_update: updates.rule,
        hook_needs_update: updates.hook,
        mcp_needs_update: updates.mcp,
        rule_path: agent_rules::display_path(a),
        rule_installed: agent_rules::is_installed(a),
        timeout_hook_supported: agent_mode::timeout_hook_supported(a),
        timeout_hook_installed: agent_mode::timeout_hook_is_installed(a),
        timeout_hook_needs_update: agent_mode::timeout_hook_supported(a)
            && (!agent_mode::timeout_hook_is_installed(a)
                || agent_mode::timeout_hook_needs_update(a)),
        recovery_hook_installed: recovery.installed && !recovery.outdated,
        permission,
        permission_needs_update,
        stop: agent_stop::status(stop_kind),
        ask_question: agent_ask_question::status(stop_kind),
        mcp_config_path: mcp_config::display_path(a),
        mcp_config_installed: mcp_config::is_installed(a),
    })
}

#[tauri::command]
pub fn agent_permission_set(
    app: tauri::AppHandle,
    agent: String,
    enabled: bool,
) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    agent_permission::set_enabled(a, enabled).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    crate::app::gui_host::refresh_integration_updates(&app);
    Ok(())
}

#[tauri::command]
pub fn agent_stop_set(agent: String, enabled: bool) -> Result<(), String> {
    let _ = parse_agent(&agent)?;
    let kind =
        crate::agents::AgentKind::parse(&agent).ok_or_else(|| "unknown agent".to_string())?;
    agent_stop::set_enabled(kind, enabled).map_err(|error| error.to_string())
}

/// 一键切换到目标模式（"none"|"cli"|"mcp"）：自动卸旧装新。
#[tauri::command]
pub fn agent_ask_question_set(agent: String, enabled: bool) -> Result<(), String> {
    let _ = parse_agent(&agent)?;
    let kind =
        crate::agents::AgentKind::parse(&agent).ok_or_else(|| "unknown agent".to_string())?;
    agent_ask_question::set_enabled(kind, enabled).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn agent_mode_set(app: tauri::AppHandle, agent: String, mode: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    let m = agent_mode::Mode::parse(&mode).ok_or_else(|| {
        crate::i18n::tr(crate::i18n::Lang::current(), "cmd.unknownMode").to_string()
    })?;
    agent_mode::set(a, m).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    crate::app::gui_host::refresh_integration_updates(&app);
    Ok(())
}

/// 把当前模式的全部产物刷新到最新（不切换模式）。
#[tauri::command]
pub fn agent_mode_update(app: tauri::AppHandle, agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    agent_mode::update(a).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    crate::app::gui_host::refresh_integration_updates(&app);
    Ok(())
}

/// 把当前模式下的单个产物（"rule" | "hook" | "mcp"）刷新到最新（不切换模式、不动其它产物）。
#[tauri::command]
pub fn agent_mode_update_artifact(
    app: tauri::AppHandle,
    agent: String,
    artifact: String,
) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    let art = agent_mode::Artifact::parse(&artifact).ok_or_else(|| {
        crate::i18n::tr(crate::i18n::Lang::current(), "cmd.unknownArtifact").to_string()
    })?;
    agent_mode::update_artifact(a, art).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    crate::app::gui_host::refresh_integration_updates(&app);
    Ok(())
}

#[tauri::command]
pub fn mcp_config_reveal(agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    mcp_config::reveal(a);
    Ok(())
}

#[tauri::command]
pub fn mcp_config_open(agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    mcp_config::open(a);
    Ok(())
}

/// 当前可执行文件绝对路径，供手动集成卡的 MCP 配置示例直接填入 `command`（与自动集成写入一致）。
#[tauri::command]
pub fn mcp_command_path() -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .map_err(|e| crate::i18n::tr(lang, "cmd.locateExeFailed").replace("{e}", &e.to_string()))
}

#[tauri::command]
pub fn agent_hook_reveal(agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    agent_mode::timeout_hook_reveal(a);
    Ok(())
}

#[tauri::command]
pub fn agent_hook_open(agent: String) -> Result<(), String> {
    let a = parse_agent(&agent)?;
    agent_mode::timeout_hook_open(a);
    Ok(())
}

// ===== Agent 生命周期追踪 hook（实验性功能） =====

use crate::agents::AgentKind;
use crate::integrations::agent_lifecycle;

fn parse_agent_kind(agent: &str) -> Result<AgentKind, String> {
    AgentKind::parse(agent).ok_or_else(|| {
        crate::i18n::tr(crate::i18n::Lang::current(), "cmd.unknownAgent").to_string()
    })
}

#[tauri::command]
pub fn agent_lifecycle_status(agent: String) -> Result<agent_lifecycle::LifecycleStatus, String> {
    let k = parse_agent_kind(&agent)?;
    Ok(agent_lifecycle::status(k))
}

#[tauri::command]
pub fn agent_lifecycle_install(app: AppHandle, agent: String) -> Result<String, String> {
    let k = parse_agent_kind(&agent)?;
    let msg = agent_lifecycle::install(k).map_err(|e| e.to_string())?;
    refresh_host_tray(&app);
    Ok(msg)
}

#[tauri::command]
pub fn agent_lifecycle_uninstall(app: AppHandle, agent: String) -> Result<String, String> {
    let k = parse_agent_kind(&agent)?;
    let msg = agent_lifecycle::uninstall(k).map_err(|e| e.to_string())?;
    refresh_host_tray(&app);
    Ok(msg)
}

/// 聚焦某 Agent 所在的终端（实验性，macOS：Terminal.app / iTerm2）。由 Agent 状态窗口逐行调用，
/// 传入该会话的 agent 进程 pid；失败（无 tty / 不支持的终端 / 未授权 / 找不到）返回 Err。
#[tauri::command]
pub fn focus_agent_terminal(pid: u32) -> Result<(), String> {
    crate::integrations::terminal_focus::focus_agent_terminal(pid)
}

/// 手动把某 agent 置为「空闲」（状态窗口纠正漏 hook 卡「工作中」场景）：向 daemon 发一条
/// `AgentForceIdle`，daemon 改状态后会经订阅推回新快照刷新窗口。即发即走、best-effort。
#[tauri::command]
pub fn agent_force_idle(session_id: String) {
    #[cfg(unix)]
    crate::client::force_agent_idle(session_id);
    #[cfg(not(unix))]
    let _ = session_id;
}

// ===== Agent 插话（spec agent-interject）=====

/// 打开某 agent 的插话 composer 窗口（AgentsView「发送消息」按钮）：路由到统一宿主
/// （每 session 全局单窗），失败兜底本进程建窗。`kind`/`cwd` 仅用于窗口头部展示。
#[tauri::command]
pub fn open_interject(
    app: AppHandle,
    session_id: String,
    kind: Option<String>,
    cwd: Option<String>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let target = crate::gui_host::InterjectTarget {
            session: session_id,
            agent: kind,
            cwd,
        };
        route_open_window(
            app,
            crate::gui_host::WindowKind::Interject,
            false,
            None,
            Some(target),
            None,
        );
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (app, session_id, kind, cwd);
        Err("unsupported".to_string())
    }
}

/// 插话窗口初始化负载：主题 + 语言 + 待送达预填全文、条数与附件。
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterjectInit {
    theme: String,
    lang: String,
    /// 待送达全文（预填编辑；空 = 无待送达）。
    text: String,
    /// 待送达条数。
    entries: usize,
    /// Flattened pending attachments. Source files are referenced; clipboard images live in the
    /// same 24-hour request-temp area used by popup answers.
    attachments: Vec<InterjectAttachmentView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterjectAttachmentView {
    path: String,
    name: String,
    size: u64,
    is_image: bool,
    available: bool,
}

impl From<crate::models::FileAttachment> for InterjectAttachmentView {
    fn from(attachment: crate::models::FileAttachment) -> Self {
        let available = std::path::Path::new(&attachment.path).is_file();
        Self {
            path: attachment.path,
            name: attachment.name,
            size: attachment.size,
            is_image: attachment.is_image,
            available,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InterjectPendingView {
    text: String,
    entries: usize,
    attachments: Vec<InterjectAttachmentView>,
}

async fn prepare_interject_attachments(
    file_paths: Vec<String>,
    pasted_images: Vec<crate::models::ImageAttachment>,
) -> Result<Vec<crate::models::FileAttachment>, String> {
    if pasted_images
        .iter()
        .any(|image| !image.media_type.starts_with("image/"))
    {
        return Err("an interjection clipboard attachment must be an image".to_string());
    }
    let pasted_images = normalize_pasted_image_filenames(pasted_images);
    let lang = crate::i18n::Lang::current();
    tauri::async_runtime::spawn_blocking(move || {
        let mut attachments = crate::cli::file_attachment::resolve(&file_paths, lang)?;
        if !pasted_images.is_empty() {
            let temp_id = format!("interject-{}", uuid::Uuid::new_v4());
            let paths = crate::cli::image_writer::save(&pasted_images, &temp_id, 0, lang)
                .map_err(|error| error.to_string())?;
            attachments.extend(crate::cli::file_attachment::resolve(&paths, lang)?);
        }
        let mut seen = std::collections::HashSet::new();
        attachments.retain(|attachment| seen.insert(attachment.path.clone()));
        Ok(attachments)
    })
    .await
    .map_err(|error| format!("interjection attachment task failed: {error}"))?
}

/// 插话窗口挂载时调用：打开到 daemon 的 composer 专属连接（登记「composer 打开中」，
/// 此后该 session 的 PreToolUse hook 挂起等待）+ 查询待送达全文作预填。连接生命周期与窗口一致。
#[tauri::command]
pub async fn interject_init(session_id: String) -> Result<InterjectInit, String> {
    // 现读配置而非 `AppState.config`（同 `todos_init`，避免常驻宿主的过期快照）。
    let config = AppConfig::load_without_secrets();
    let theme = theme_str(config.general.theme);
    let lang = crate::i18n::Lang::resolve(&config.general.language)
        .code()
        .to_string();
    #[cfg(unix)]
    let (text, entries, attachments) = crate::client::composer::open(&session_id).await;
    #[cfg(not(unix))]
    let (text, entries, attachments) = {
        let _ = &session_id;
        (
            String::new(),
            0usize,
            Vec::<crate::models::FileAttachment>::new(),
        )
    };
    Ok(InterjectInit {
        theme,
        lang,
        text,
        entries,
        attachments: attachments
            .into_iter()
            .map(InterjectAttachmentView::from)
            .collect(),
    })
}

/// 插话提交（整体覆盖该 session 的待送达队列；空文本＝清空）：优先经 composer 连接送出
/// （等待中的 hook 可当场拿到消息），随后关连接、关窗口。
#[tauri::command]
pub async fn interject_submit(
    app: AppHandle,
    session_id: String,
    text: String,
    file_paths: Option<Vec<String>>,
    pasted_images: Option<Vec<crate::models::ImageAttachment>>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let attachments = prepare_interject_attachments(
            file_paths.unwrap_or_default(),
            pasted_images.unwrap_or_default(),
        )
        .await?;
        crate::client::composer::submit(&session_id, &text, attachments).await;
        crate::client::composer::close(&session_id);
        close_interject_window(&app, &session_id);
    }
    #[cfg(not(unix))]
    let _ = (app, session_id, text, file_paths, pasted_images);
    Ok(())
}

/// 插话取消（取消按钮 / Esc）：关连接（daemon 视为 composer 关闭，放行等待 hook）、关窗口。
/// 队列不动（已排队消息保留）。
#[tauri::command]
pub fn interject_cancel(app: AppHandle, session_id: String) {
    #[cfg(unix)]
    {
        crate::client::composer::close(&session_id);
        close_interject_window(&app, &session_id);
    }
    #[cfg(not(unix))]
    let _ = (app, session_id);
}

/// 撤回某 session 的全部待送达插话（AgentsView 徽标撤回按钮）。即发即走、best-effort；
/// daemon 清空后经订阅推回新快照（徽标消失）。
#[tauri::command]
pub fn interject_clear(session_id: String) {
    #[cfg(unix)]
    crate::client::report_agent_event(crate::ipc::ClientMsg::InterjectClear { session_id });
    #[cfg(not(unix))]
    let _ = session_id;
}

// ===== Agent 控制台（spec gui-agent-console）=====

/// 控制台焦点会话（C8）：daemon 对焦点会话按签名推 `agent-detail` 帧；`None`/空串＝取消焦点。
#[tauri::command]
pub fn agents_focus(session_id: Option<String>) {
    #[cfg(unix)]
    crate::app::set_agents_focus(session_id.filter(|s| !s.trim().is_empty()));
    #[cfg(not(unix))]
    let _ = session_id;
}

/// 「去回答」（C7）：请求 daemon 聚焦对应请求的弹窗（托盘「待答」子菜单同款链路，即发即走）。
#[tauri::command]
pub fn focus_request(request_id: String) {
    #[cfg(unix)]
    crate::client::report_agent_event(crate::ipc::ClientMsg::FocusRequest { request_id });
    #[cfg(not(unix))]
    let _ = request_id;
}

/// 控制台输入框发消息（C3 追加语义，同 IM `/msg`）：不覆盖既有待送达队列，即发即走。
#[tauri::command]
pub async fn interject_append(
    session_id: String,
    text: String,
    file_paths: Option<Vec<String>>,
    pasted_images: Option<Vec<crate::models::ImageAttachment>>,
) -> Result<(), String> {
    #[cfg(unix)]
    {
        let attachments = prepare_interject_attachments(
            file_paths.unwrap_or_default(),
            pasted_images.unwrap_or_default(),
        )
        .await?;
        crate::client::report_agent_event(crate::ipc::ClientMsg::InterjectAppend {
            session_id,
            text,
            attachments,
        });
    }
    #[cfg(not(unix))]
    let _ = (session_id, text, file_paths, pasted_images);
    Ok(())
}

/// 待送达气泡内容查询（C3）：返回全文、条数与附件；daemon 未运行 → 空状态。
#[tauri::command]
pub async fn interject_peek(session_id: String) -> Result<InterjectPendingView, String> {
    #[cfg(unix)]
    {
        let (text, entries, attachments) = crate::client::interject_peek(session_id).await;
        Ok(InterjectPendingView {
            text,
            entries,
            attachments: attachments
                .into_iter()
                .map(InterjectAttachmentView::from)
                .collect(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = session_id;
        Ok(InterjectPendingView {
            text: String::new(),
            entries: 0,
            attachments: Vec::new(),
        })
    }
}

/// 控制台 git 调用闸门（C16）：同一时刻只跑一个 git 任务；抢不到锁（上次未返回）直接
/// 返回 busy 由前端跳过本次刷新，另有 10s 硬超时兜底慢仓库。
#[cfg(unix)]
fn diff_gate() -> &'static tokio::sync::Mutex<()> {
    static GATE: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    GATE.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[cfg(unix)]
async fn run_git_bounded<T, F>(f: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let Ok(_guard) =
        tokio::time::timeout(std::time::Duration::from_millis(200), diff_gate().lock()).await
    else {
        return Err("busy".to_string());
    };
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::task::spawn_blocking(f),
    )
    .await
    .map_err(|_| "git timeout".to_string())?
    .map_err(|e| e.to_string())?
}

/// 项目未暂存变更统计（C15 第一级）：状态条数据。`project` 为任意项目内目录（映射 git 根）。
#[tauri::command]
pub async fn console_diff_stat(project: String) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    {
        run_git_bounded(move || {
            let root = crate::gitutil::find_git_root(std::path::Path::new(&project))
                .ok_or_else(|| "not a git repository".to_string())?;
            let files = crate::gitutil::diff_stat(&root)?;
            Ok(serde_json::json!({
                "root": root.to_string_lossy(),
                "files": files,
            }))
        })
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = project;
        Err("unsupported platform".to_string())
    }
}

/// 单文件 hunk 视图（C15 第二级，展开时才调）。`path` 应来自 `console_diff_stat` 输出。
#[tauri::command]
pub async fn console_diff_file(project: String, path: String) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    {
        run_git_bounded(move || {
            use crate::gitutil::{FileChangeKind, LineKind};
            let root = crate::gitutil::find_git_root(std::path::Path::new(&project))
                .ok_or_else(|| "not a git repository".to_string())?;
            let f = crate::gitutil::diff_file(&root, &path)?;
            let lines: Vec<serde_json::Value> = f
                .lines
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "kind": match l.kind {
                            LineKind::Insert => "add",
                            LineKind::Delete => "del",
                            LineKind::Header => "header",
                            LineKind::Equal => "context",
                        },
                        "text": l.text,
                    })
                })
                .collect();
            Ok(serde_json::json!({
                "path": f.path,
                "kind": match f.kind {
                    FileChangeKind::Untracked => "A",
                    FileChangeKind::Deleted => "D",
                    FileChangeKind::Binary => "B",
                    FileChangeKind::Modified => "M",
                },
                "skipped": f.skipped,
                "skipReason": f.skip_reason,
                "lines": lines,
            }))
        })
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (project, path);
        Err("unsupported platform".to_string())
    }
}

/// 暂存指定路径（C15）：单文件与「全部暂存」共用（后者传全量路径）。返回实际暂存数。
#[tauri::command]
pub async fn console_stage(project: String, paths: Vec<String>) -> Result<usize, String> {
    #[cfg(unix)]
    {
        run_git_bounded(move || {
            let root = crate::gitutil::find_git_root(std::path::Path::new(&project))
                .ok_or_else(|| "not a git repository".to_string())?;
            let r = crate::gitutil::stage_paths(&root, &paths)?;
            Ok(r.paths.len())
        })
        .await
    }
    #[cfg(not(unix))]
    {
        let _ = (project, paths);
        Err("unsupported platform".to_string())
    }
}

/// 完整会话分页缓存：`(kind, session) → (transcript mtime, 解析结果)`。mtime 变即重解析
/// （解析受 `MAX_READ_BYTES` 上限保护，重解析成本有界）。
#[cfg(unix)]
type TxCache = std::collections::HashMap<
    (String, String),
    (
        std::time::SystemTime,
        std::sync::Arc<crate::agents::transcript_full::TranscriptDoc>,
    ),
>;

#[cfg(unix)]
fn tx_cache() -> &'static std::sync::Mutex<TxCache> {
    static CACHE: std::sync::OnceLock<std::sync::Mutex<TxCache>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// 控制台完整会话分页（C14）：`before` 为事件绝对下标游标（None＝末尾），返回
/// `[max(0, before-limit), before)` 窗口 + 总数与截断标志；每页默认 200 条。
#[tauri::command]
pub async fn console_transcript(
    kind: String,
    session_id: String,
    before: Option<usize>,
    limit: Option<usize>,
) -> Result<serde_json::Value, String> {
    #[cfg(unix)]
    {
        use crate::agents::transcript_full;
        let agent = crate::agents::AgentKind::parse(&kind).ok_or("unknown agent kind")?;
        let limit = limit.unwrap_or(200).clamp(1, 500);
        tokio::task::spawn_blocking(move || -> Result<serde_json::Value, String> {
            let mtime = transcript_full::transcript_mtime(agent, &session_id)
                .ok_or_else(|| "transcript not found".to_string())?;
            let key = (kind.clone(), session_id.clone());
            let cached = {
                let cache = tx_cache().lock().unwrap();
                cache
                    .get(&key)
                    .filter(|(t, _)| *t == mtime)
                    .map(|(_, doc)| doc.clone())
            };
            let doc = match cached {
                Some(doc) => doc,
                None => {
                    let doc =
                        std::sync::Arc::new(transcript_full::load_events(agent, &session_id)?);
                    let mut cache = tx_cache().lock().unwrap();
                    // 缓存有界：超过 8 个会话时整体清空（控制台常用 1–2 个，简单粗暴即可）。
                    if cache.len() >= 8 {
                        cache.clear();
                    }
                    cache.insert(key, (mtime, doc.clone()));
                    doc
                }
            };
            let total = doc.events.len();
            let end = before.unwrap_or(total).min(total);
            let start = end.saturating_sub(limit);
            let events: Vec<serde_json::Value> = doc.events[start..end]
                .iter()
                .map(transcript_full::event_json)
                .collect();
            Ok(serde_json::json!({
                "events": events,
                "start": start,
                "total": total,
                "truncatedHead": doc.truncated_head,
                "partial": doc.partial,
            }))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(not(unix))]
    {
        let _ = (kind, session_id, before, limit);
        Err("unsupported platform".to_string())
    }
}

/// 关闭某 session 的插话窗口（提交/取消后收尾）。窗口不存在时静默。
#[cfg(unix)]
fn close_interject_window(app: &AppHandle, session_id: &str) {
    let label = crate::gui_host::interject_label(session_id);
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.close();
    }
}

/// 生命周期 hook 装/卸后刷新托盘菜单，使「Agent 状态」入口随之显隐。仅在统一 GUI 宿主进程内
/// （持有 `HostState`）实际生效；其它进程自动 no-op。
fn refresh_host_tray(app: &AppHandle) {
    #[cfg(unix)]
    {
        let app2 = app.clone();
        let _ = app.run_on_main_thread(move || crate::app::gui_host::refresh_tray(&app2));
    }
    #[cfg(not(unix))]
    {
        let _ = app;
    }
}

// ===== Telegram 测试连接 =====

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramTestArgs {
    pub bot_token: String,
    pub chat_id: String,
    pub api_base_url: String,
}

#[tauri::command]
pub async fn telegram_test(args: TelegramTestArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    let bot_token = fallback_secret(&args.bot_token, |c| c.channels.telegram.bot_token.clone());
    let client = TelegramClient::new(bot_token, args.chat_id, args.api_base_url)
        .map_err(|e| e.localized(lang))?;
    client
        .test_connection(lang)
        .await
        .map_err(|e| e.localized(lang))
}

// ===== 钉钉测试连接 / userId 自动识别 =====

use crate::config::DingTalkChannelConfig;
use crate::dingtalk::client::DingTalkClient;
use crate::dingtalk::stream::{StreamConn, StreamEvent, TOPIC_BOT_MESSAGE};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DingTalkTestArgs {
    pub client_id: String,
    pub client_secret: String,
    pub user_id: String,
}

/// 测试连接：换 token（校验 ClientId/Secret）+ 向 userId 单聊发一条测试消息。
#[tauri::command]
pub async fn dingtalk_test(args: DingTalkTestArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    if args.user_id.trim().is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillUserId").to_string());
    }
    let client_secret = fallback_secret(&args.client_secret, |c| {
        c.channels.dingding.client_secret.clone()
    });
    let cfg = DingTalkChannelConfig {
        enabled: true,
        client_id: args.client_id,
        client_secret,
        user_id: args.user_id,
        card_template_id: String::new(),
        ..Default::default()
    };
    let client = DingTalkClient::new(&cfg).map_err(|e| e.localized(lang))?;
    client
        .send_oto_text(crate::i18n::tr(lang, "cmd.ddTestRemote"))
        .await
        .map_err(|e| e.localized(lang))?;
    Ok(crate::i18n::tr(lang, "cmd.ddTestSent").to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DingTalkDetectArgs {
    pub client_id: String,
    pub client_secret: String,
}

/// 自动识别准备：校验 ClientId/Secret（换 token），通过后返回供用户私聊发送的 4 位识别码。
/// 校验不通过则返回中文错误（前端据此不展示识别码、不进入等待）。
#[tauri::command]
pub async fn dingtalk_detect_prepare(args: DingTalkDetectArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    let client_id = args.client_id.trim();
    let secret = fallback_secret(&args.client_secret, |c| {
        c.channels.dingding.client_secret.clone()
    });
    let client_secret = secret.trim();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillClientIdSecret").to_string());
    }
    let http = reqwest::Client::new();
    crate::dingtalk::token::get_token(&http, client_id, client_secret)
        .await
        .map_err(|e| e.localized(lang))?;
    Ok(gen_detect_code())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DingTalkWaitArgs {
    pub client_id: String,
    pub client_secret: String,
    pub code: String,
}

/// 自动识别等待：开 Stream（bot 消息 topic），等到内容等于识别码的单聊消息，返回其 senderStaffId。
/// 120 秒超时报错。
#[tauri::command]
pub async fn dingtalk_detect_wait(args: DingTalkWaitArgs) -> Result<String, String> {
    use std::time::Duration;
    let lang = crate::i18n::Lang::current();
    let client_id = args.client_id.trim().to_string();
    let secret = fallback_secret(&args.client_secret, |c| {
        c.channels.dingding.client_secret.clone()
    });
    let client_secret = secret.trim().to_string();
    if client_id.is_empty() || client_secret.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillClientIdSecret").to_string());
    }
    let code = args.code.trim().to_string();
    if code.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.detectCodeInvalid").to_string());
    }

    detect_with_cancel(lang, async move {
        // Q6：经 Daemon 长连接识别（避免与 Daemon 单连接冲突）。Daemon 接管即用其结果；
        // 接不通 Daemon 才回退进程内临时连接（非 Unix 无 Daemon，直接走回退）。
        #[cfg(unix)]
        {
            let req = crate::ipc::DetectRequest {
                kind: "dingtalk".to_string(),
                app_key: client_id.clone(),
                app_secret: client_secret.clone(),
                base_url: String::new(),
                code: code.clone(),
                lang: lang.code().to_string(),
            };
            if let Some(result) = crate::client::request_detect(req).await {
                return result;
            }
        }

        let http = reqwest::Client::new();
        let mut stream =
            StreamConn::connect(http, &client_id, &client_secret, &[TOPIC_BOT_MESSAGE])
                .await
                .map_err(|e| e.localized(lang))?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string());
            }
            match tokio::time::timeout(remaining, stream.recv()).await {
                Ok(Some(StreamEvent::BotMessage(data))) => {
                    let content = data
                        .get("text")
                        .and_then(|t| t.get("content"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("")
                        .trim();
                    if content == code {
                        if let Some(sender) = data.get("senderStaffId").and_then(|v| v.as_str()) {
                            return Ok(sender.to_string());
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(crate::i18n::tr(lang, "cmd.streamDisconnected").to_string()),
                Err(_) => return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string()),
            }
        }
    })
    .await
}

// ===== 飞书测试连接 / open_id 自动识别 =====

use crate::config::FeishuChannelConfig;
use crate::feishu::client::FeishuClient;
use crate::feishu::ws::{FeishuWs, WsEvent};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeishuTestArgs {
    pub app_id: String,
    pub app_secret: String,
    pub open_id: String,
    pub base_url: String,
}

/// 测试连接：换 token（校验 AppId/Secret）+ 向 open_id 单聊发一条测试消息。
#[tauri::command]
pub async fn feishu_test(args: FeishuTestArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    if args.open_id.trim().is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillOpenId").to_string());
    }
    let app_secret = fallback_secret(&args.app_secret, |c| c.channels.feishu.app_secret.clone());
    let cfg = FeishuChannelConfig {
        enabled: true,
        app_id: args.app_id,
        app_secret,
        open_id: args.open_id,
        base_url: args.base_url,
    };
    let client = FeishuClient::new(&cfg).map_err(|e| e.localized(lang))?;
    client
        .send_text(crate::i18n::tr(lang, "cmd.fsTestRemote"))
        .await
        .map_err(|e| e.localized(lang))?;
    Ok(crate::i18n::tr(lang, "cmd.fsTestSent").to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeishuDetectArgs {
    pub app_id: String,
    pub app_secret: String,
    pub base_url: String,
}

/// 自动识别准备：校验 AppId/Secret（换 token），通过后返回供用户私聊发送的 4 位识别码。
#[tauri::command]
pub async fn feishu_detect_prepare(args: FeishuDetectArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    let app_id = args.app_id.trim();
    let secret = fallback_secret(&args.app_secret, |c| c.channels.feishu.app_secret.clone());
    let app_secret = secret.trim();
    if app_id.is_empty() || app_secret.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillAppIdSecret").to_string());
    }
    let base_url = effective_feishu_base(&args.base_url);
    let http = reqwest::Client::new();
    crate::feishu::token::get_token(&http, &base_url, app_id, app_secret)
        .await
        .map_err(|e| e.localized(lang))?;
    Ok(gen_detect_code())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeishuWaitArgs {
    pub app_id: String,
    pub app_secret: String,
    pub base_url: String,
    pub code: String,
}

/// 自动识别等待：开长连接，等到内容等于识别码的单聊消息，返回发送者 open_id。120 秒超时报错。
#[tauri::command]
pub async fn feishu_detect_wait(args: FeishuWaitArgs) -> Result<String, String> {
    use std::time::Duration;
    let lang = crate::i18n::Lang::current();
    let app_id = args.app_id.trim().to_string();
    let secret = fallback_secret(&args.app_secret, |c| c.channels.feishu.app_secret.clone());
    let app_secret = secret.trim().to_string();
    if app_id.is_empty() || app_secret.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillAppIdSecret").to_string());
    }
    let code = args.code.trim().to_string();
    if code.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.detectCodeInvalid").to_string());
    }
    let base_url = effective_feishu_base(&args.base_url);

    detect_with_cancel(lang, async move {
        // Q6：经 Daemon 长连接识别（见钉钉同段说明）。
        #[cfg(unix)]
        {
            let req = crate::ipc::DetectRequest {
                kind: "feishu".to_string(),
                app_key: app_id.clone(),
                app_secret: app_secret.clone(),
                base_url: base_url.clone(),
                code: code.clone(),
                lang: lang.code().to_string(),
            };
            if let Some(result) = crate::client::request_detect(req).await {
                return result;
            }
        }

        let http = reqwest::Client::new();
        let mut ws = FeishuWs::connect(http, &base_url, &app_id, &app_secret)
            .await
            .map_err(|e| e.localized(lang))?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string());
            }
            match tokio::time::timeout(remaining, ws.recv()).await {
                Ok(Some(WsEvent::Message(event))) => {
                    if let Some((open_id, text)) = feishu_text_and_sender(&event) {
                        if text.trim() == code {
                            return Ok(open_id);
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(crate::i18n::tr(lang, "cmd.streamDisconnected").to_string()),
                Err(_) => return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string()),
            }
        }
    })
    .await
}

/// base_url 缺省回退飞书国内。
fn effective_feishu_base(base_url: &str) -> String {
    let b = base_url.trim().trim_end_matches('/');
    if b.is_empty() {
        "https://open.feishu.cn".to_string()
    } else {
        b.to_string()
    }
}

/// 从 im.message.receive_v1 的 event 取 (发送者 open_id, 文本内容)。非文本消息返回 None。
fn feishu_text_and_sender(event: &serde_json::Value) -> Option<(String, String)> {
    let open_id = event
        .get("sender")
        .and_then(|s| s.get("sender_id"))
        .and_then(|i| i.get("open_id"))
        .and_then(|v| v.as_str())?
        .to_string();
    let message = event.get("message")?;
    if message.get("message_type").and_then(|v| v.as_str()) != Some("text") {
        return None;
    }
    let content_str = message
        .get("content")
        .and_then(|v| v.as_str())
        .unwrap_or("{}");
    let content: serde_json::Value = serde_json::from_str(content_str).ok()?;
    let text = content.get("text").and_then(|v| v.as_str())?.to_string();
    Some((open_id, text))
}

// ===== Slack 测试连接 / userId 自动识别 =====

use crate::config::SlackChannelConfig;
use crate::slack::client::SlackClient;
use crate::slack::ws::{self as slack_ws, SlackWs, WsEvent as SlackWsEvent};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlackTestArgs {
    pub bot_token: String,
    pub app_token: String,
    pub user_id: String,
}

/// 测试连接：校验 Bot Token（auth.test + 向 userId 发测试 DM）+ 校验 App Token（apps.connections.open）。
#[tauri::command]
pub async fn slack_test(args: SlackTestArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    if args.user_id.trim().is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillUserId").to_string());
    }
    let bot_token = fallback_secret(&args.bot_token, |c| c.channels.slack.bot_token.clone());
    let app_token = fallback_secret(&args.app_token, |c| c.channels.slack.app_token.clone());
    let cfg = SlackChannelConfig {
        enabled: true,
        bot_token,
        app_token,
        user_id: args.user_id,
    };
    let client = SlackClient::new(&cfg).map_err(|e| e.localized(lang))?;
    // Bot Token：auth.test + 解析 DM + 发测试消息。
    client.auth_test().await.map_err(|e| e.localized(lang))?;
    let dm = client.open_dm().await.map_err(|e| e.localized(lang))?;
    client
        .post_text(&dm, crate::i18n::tr(lang, "cmd.slTestRemote"))
        .await
        .map_err(|e| e.localized(lang))?;
    // App Token：能拿到 wss 即通过（不保持长连）。
    slack_ws::open_socket_url(client.http(), client.app_token())
        .await
        .map_err(|e| e.localized(lang))?;
    Ok(crate::i18n::tr(lang, "cmd.slTestSent").to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlackDetectArgs {
    pub bot_token: String,
    pub app_token: String,
}

/// 自动识别准备：校验双 token（App Token 能开 Socket Mode）后返回供用户私聊发送的 4 位识别码。
#[tauri::command]
pub async fn slack_detect_prepare(args: SlackDetectArgs) -> Result<String, String> {
    let lang = crate::i18n::Lang::current();
    let bot_token = fallback_secret(&args.bot_token, |c| c.channels.slack.bot_token.clone());
    let app_token = fallback_secret(&args.app_token, |c| c.channels.slack.app_token.clone());
    if bot_token.trim().is_empty() || app_token.trim().is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillSlackTokens").to_string());
    }
    let http = reqwest::Client::new();
    slack_ws::open_socket_url(&http, app_token.trim())
        .await
        .map_err(|e| e.localized(lang))?;
    Ok(gen_detect_code())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SlackWaitArgs {
    pub bot_token: String,
    pub app_token: String,
    pub code: String,
}

/// 自动识别等待：开 Socket Mode，等到 DM 文本内容等于识别码的消息，返回发送者 user id。120 秒超时报错。
#[tauri::command]
pub async fn slack_detect_wait(args: SlackWaitArgs) -> Result<String, String> {
    use std::time::Duration;
    let lang = crate::i18n::Lang::current();
    let bot_token = fallback_secret(&args.bot_token, |c| c.channels.slack.bot_token.clone());
    let app_token = fallback_secret(&args.app_token, |c| c.channels.slack.app_token.clone());
    if bot_token.trim().is_empty() || app_token.trim().is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.fillSlackTokens").to_string());
    }
    let code = args.code.trim().to_string();
    if code.is_empty() {
        return Err(crate::i18n::tr(lang, "cmd.detectCodeInvalid").to_string());
    }

    detect_with_cancel(lang, async move {
        // Q6：经 Daemon 长连接识别（见钉钉/飞书同段说明）。app_key=App Token（Socket 复用键），
        // app_secret=Bot Token（建连时校验齐全）。
        #[cfg(unix)]
        {
            let req = crate::ipc::DetectRequest {
                kind: "slack".to_string(),
                app_key: app_token.trim().to_string(),
                app_secret: bot_token.trim().to_string(),
                base_url: String::new(),
                code: code.clone(),
                lang: lang.code().to_string(),
            };
            if let Some(result) = crate::client::request_detect(req).await {
                return result;
            }
        }

        let http = reqwest::Client::new();
        let mut ws = SlackWs::connect(http, app_token.trim())
            .await
            .map_err(|e| e.localized(lang))?;

        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string());
            }
            match tokio::time::timeout(remaining, ws.recv()).await {
                Ok(Some(SlackWsEvent::Message(event))) => {
                    if let Some((user, text)) = slack_text_and_sender(&event) {
                        if text.trim() == code {
                            return Ok(user);
                        }
                    }
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(crate::i18n::tr(lang, "cmd.streamDisconnected").to_string()),
                Err(_) => return Err(crate::i18n::tr(lang, "cmd.detectTimeout").to_string()),
            }
        }
    })
    .await
}

/// 从 Slack message 事件取 (发送者 user id, 文本内容)。无文本返回 None。
fn slack_text_and_sender(event: &serde_json::Value) -> Option<(String, String)> {
    let user = event.get("user").and_then(|v| v.as_str())?.to_string();
    let text = event.get("text").and_then(|v| v.as_str())?.to_string();
    Some((user, text))
}

// ===== 自动识别「取消」支持（三家共用） =====
//
// 识别的「等待」步骤最多阻塞 120s。UI 加了「取消」按钮，但 Tauri 命令本身不可中断，所以这里
// 用一个进程内取消信号：每次 `*_detect_wait` 注册一个新 `Notify`，命令体经 `detect_with_cancel`
// 与 `notified()` 竞速；`detect_cancel` 命令置位即让等待提前返回并 **drop 掉等待 future**——
// 对走 daemon 的路径这会关掉到 daemon 的连接（daemon 侧 `handle_detect` 随之中止并释放临时长连接），
// 对进程内回退路径则直接 drop 临时 WS。UI 同一时刻只有一个识别在跑，故全局单槽即可。

static DETECT_CANCEL: std::sync::OnceLock<
    std::sync::Mutex<Option<std::sync::Arc<tokio::sync::Notify>>>,
> = std::sync::OnceLock::new();

fn detect_cancel_slot() -> &'static std::sync::Mutex<Option<std::sync::Arc<tokio::sync::Notify>>> {
    DETECT_CANCEL.get_or_init(|| std::sync::Mutex::new(None))
}

/// 为当前识别注册一个新的取消令牌（替换任何旧令牌）。
fn detect_cancel_register() -> std::sync::Arc<tokio::sync::Notify> {
    let token = std::sync::Arc::new(tokio::sync::Notify::new());
    *detect_cancel_slot().lock().unwrap() = Some(token.clone());
    token
}

/// 身份安全地清槽：仅当槽里仍是本次令牌时才清（避免误清后一次识别的令牌）。
fn detect_cancel_clear(token: &std::sync::Arc<tokio::sync::Notify>) {
    let mut guard = detect_cancel_slot().lock().unwrap();
    if matches!(guard.as_ref(), Some(cur) if std::sync::Arc::ptr_eq(cur, token)) {
        *guard = None;
    }
}

/// 跑识别 `work`，直到完成或被取消；取消时返回本地化的「已取消」。
async fn detect_with_cancel<F>(lang: crate::i18n::Lang, work: F) -> Result<String, String>
where
    F: std::future::Future<Output = Result<String, String>>,
{
    let token = detect_cancel_register();
    let out = tokio::select! {
        result = work => result,
        _ = token.notified() => Err(crate::i18n::tr(lang, "cmd.detectCancelled").to_string()),
    };
    detect_cancel_clear(&token);
    out
}

/// 取消正在进行的「自动识别」等待（UI「取消」按钮调用）。无进行中识别则无操作。
#[tauri::command]
pub fn detect_cancel() {
    if let Some(token) = detect_cancel_slot().lock().unwrap().take() {
        token.notify_one();
    }
}

/// 生成 4 位识别码（瞬时配对用，无需强随机）。
fn gen_detect_code() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    format!("{:04}", nanos % 10000)
}

// ===== 版本自更新（self-update） =====

/// daemon 经 GUI Helper 推送的自更新态（弹窗进程内缓存，规避「事件早于前端监听」的竞态）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushedUpdateState {
    pub available: bool,
    pub latest_version: String,
    pub pending: bool,
}

static PUSHED_UPDATE: std::sync::OnceLock<std::sync::Mutex<PushedUpdateState>> =
    std::sync::OnceLock::new();

fn pushed_update_slot() -> &'static std::sync::Mutex<PushedUpdateState> {
    PUSHED_UPDATE.get_or_init(|| std::sync::Mutex::new(PushedUpdateState::default()))
}

/// GUI Helper 读到 daemon 的 `UpdateState` 后写入此缓存（供弹窗挂载时拉取初值）。
pub fn set_pushed_update(state: PushedUpdateState) {
    if let Ok(mut slot) = pushed_update_slot().lock() {
        *slot = state;
    }
}

/// 弹窗挂载时拉取「已推送的自更新态」初值（之后变化经事件实时更新）。
#[tauri::command]
pub fn popup_update_state() -> PushedUpdateState {
    pushed_update_slot()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

/// 本地当前版本（编译期嵌入）。
#[tauri::command]
pub fn get_app_version() -> String {
    crate::update::current_version()
}

/// Check the latest stable release and immediately synchronize a successful result into the GUI
/// Host and daemon snapshot. Manual checks revalidate upstream caches and clear dismissed versions
/// in the same persisted-state transaction.
#[tauri::command]
pub async fn update_check(
    app: AppHandle,
    manual: bool,
) -> Result<crate::update::UpdateInfo, String> {
    let checked = if manual {
        crate::update::check_fresh().await
    } else {
        crate::update::check().await
    };
    let info = match checked {
        Ok(info) => crate::update::persist_check_result(info, manual),
        Err(error) => {
            #[cfg(unix)]
            if manual {
                crate::app::gui_host::sync_update_check_error(&app, &error.to_string());
            }
            return Err(error.to_string());
        }
    };
    #[cfg(unix)]
    crate::app::gui_host::sync_checked_update(&app, &info, manual);
    #[cfg(unix)]
    crate::client::notify_update_state_changed().await;
    Ok(info)
}

/// 取指定版本（tag `v<version>`）的更新日志（关于区「查看当前版本更新日志」用）。
#[tauri::command]
pub async fn update_get_version_notes(version: String) -> Result<String, String> {
    crate::update::notes::notes_for_tag(&version)
        .await
        .map_err(|e| e.to_string())
}

/// 取更新日志：`aggregate=true` 聚合「当前版本→最新版本」之间所有版本（懒加载）。
#[tauri::command]
pub async fn update_get_notes(aggregate: bool) -> Result<String, String> {
    if !aggregate {
        return crate::update::notes::latest_notes()
            .await
            .map_err(|e| e.to_string());
    }
    let current = crate::update::current_version();
    let to = {
        let st = crate::update::state::load();
        if st.latest_version.is_empty() {
            crate::update::check()
                .await
                .map_err(|e| e.to_string())?
                .latest_version
        } else {
            st.latest_version
        }
    };
    crate::update::notes::aggregated_notes(&current, &to)
        .await
        .map_err(|e| e.to_string())
}

/// 应用更新：把新二进制落盘（不 restart；换新交给 daemon drain）。下载进度经
/// `update_download_progress` 事件回传；完成发 `update_apply_finished`。
#[tauri::command]
pub async fn update_apply(app: AppHandle) -> Result<(), String> {
    let updater = crate::update::select_updater();
    let app_for_cb = app.clone();
    let cb: crate::update::ProgressCb = Box::new(move |p| {
        let _ = app_for_cb.emit("update_download_progress", p);
    });
    updater.apply(Some(cb)).await.map_err(|e| e.to_string())?;
    crate::update::state::set_pending(true);
    #[cfg(unix)]
    crate::client::notify_update_applied().await;
    let _ = app.emit("update_apply_finished", ());
    Ok(())
}

/// 忽略某版本（不再主动弹该版本提示；设置内手动检查可重置）。
#[tauri::command]
pub fn update_dismiss(version: String) {
    crate::update::state::dismiss(&version);
}

/// 设置内更新后「重启设置页面」：用新二进制重开设置进程，再退出当前设置窗。
#[tauri::command]
pub fn restart_settings(app: AppHandle) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let lang = crate::i18n::Lang::current();
    let exe = std::env::current_exe()
        .map_err(|e| crate::i18n::tr(lang, "cmd.locateExeFailed").replace("{e}", &e.to_string()))?;
    Command::new(exe)
        .arg("--settings")
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| crate::i18n::tr(lang, "cmd.openFailed").replace("{e}", &e.to_string()))?;
    app.exit(0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(path: &str, pinned: bool, hidden: bool) -> crate::agents::workspaces::Workspace {
        crate::agents::workspaces::Workspace {
            path: path.to_string(),
            label: path.rsplit('/').next().unwrap_or(path).to_string(),
            last_used_at: 0,
            agents: Vec::new(),
            pinned,
            hidden,
        }
    }

    #[test]
    fn pasted_image_names_are_unique_and_strip_path_components() {
        let image = |filename: Option<&str>| crate::models::ImageAttachment {
            data: "data:image/png;base64,aGVsbG8=".to_string(),
            media_type: "image/png".to_string(),
            filename: filename.map(str::to_string),
        };
        let images = normalize_pasted_image_filenames(vec![
            image(Some("/tmp/capture.png")),
            image(Some("capture.png")),
            image(Some("  ")),
            image(None),
        ]);
        assert_eq!(images[0].filename.as_deref(), Some("1-capture.png"));
        assert_eq!(images[1].filename.as_deref(), Some("2-capture.png"));
        assert_eq!(images[2].filename, None);
        assert_eq!(images[3].filename, None);
    }

    #[test]
    fn pasted_image_validation_allows_empty_drafts_and_rejects_invalid_payloads() {
        let image = crate::models::ImageAttachment {
            data: "data:image/png;base64,aGVsbG8=".to_string(),
            media_type: "image/png".to_string(),
            filename: Some("capture.png".to_string()),
        };
        assert!(validate_pasted_images(&[]).is_ok());
        let at_limit = vec![image.clone(); crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO];
        assert!(validate_pasted_images(&at_limit).is_ok());
        let over_limit = vec![image.clone(); crate::todo_attachments::MAX_ATTACHMENTS_PER_TODO + 1];
        assert!(validate_pasted_images(&over_limit).is_err());

        let mut invalid_media = image;
        invalid_media.media_type = "application/octet-stream".to_string();
        assert!(validate_pasted_images(&[invalid_media]).is_err());
    }

    #[tokio::test]
    async fn interject_attachments_reference_files_and_materialize_clipboard_images_temporarily() {
        let source_dir = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("notes.txt");
        std::fs::write(&source, b"hello").unwrap();
        let image = crate::models::ImageAttachment {
            data: "data:image/png;base64,aGVsbG8=".to_string(),
            media_type: "image/png".to_string(),
            filename: Some("capture.png".to_string()),
        };

        let attachments =
            prepare_interject_attachments(vec![source.to_string_lossy().into_owned()], vec![image])
                .await
                .unwrap();

        assert_eq!(attachments.len(), 2);
        assert_eq!(
            attachments[0].path,
            std::fs::canonicalize(&source).unwrap().to_string_lossy()
        );
        assert!(std::path::Path::new(&attachments[1].path).is_file());
        assert!(attachments[1].is_image);
        let request_dir = std::path::Path::new(&attachments[1].path)
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap();
        assert!(request_dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("interject-"));
        let _ = std::fs::remove_dir_all(request_dir);
    }

    #[tokio::test]
    async fn interject_attachments_reject_non_image_clipboard_payloads() {
        let payload = crate::models::ImageAttachment {
            data: "aGVsbG8=".to_string(),
            media_type: "application/octet-stream".to_string(),
            filename: Some("payload.bin".to_string()),
        };
        let error = prepare_interject_attachments(Vec::new(), vec![payload])
            .await
            .unwrap_err();
        assert!(error.contains("must be an image"));
    }

    /// spec gui-agent-task-launch §4：workspace 顺序保持索引序在前，待办独有项目排后；
    /// 按路径去重；hidden 与不存在目录被过滤。
    #[test]
    fn new_task_projects_order_dedup_and_filters() {
        let root = tempfile::tempdir().unwrap();
        let make = |name: &str| {
            let dir = root.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            dir.to_string_lossy().to_string()
        };
        let a = make("a");
        let b = make("b");
        let c = make("c");
        let gone = root.path().join("gone").to_string_lossy().to_string();

        let out = build_new_task_projects(
            vec![
                workspace(&b, true, false),
                workspace(&a, false, false),
                workspace(&c, false, true),     // hidden → 不列
                workspace(&gone, false, false), // 不存在 → 不列
            ],
            vec![a.clone(), c.clone(), gone.clone(), c.clone()],
        );
        let paths: Vec<_> = out.iter().map(|p| p.path.as_str()).collect();
        // workspace 段保持传入顺序（b 置顶在前来自索引排序）；a 在待办段去重；
        // c 虽为 hidden workspace，但有待办 → 以待办项目身份列出（且 dedup 一次）。
        assert_eq!(paths, vec![b.as_str(), a.as_str(), c.as_str()]);
        assert_eq!(out[0].source, "workspace");
        assert!(out[0].pinned);
        assert_eq!(out[2].source, "todos");
    }

    /// spec gui-agent-task-launch §2.5：非法入参（未知 agent / 未消解权限 / NUL）在任何
    /// 副作用（record 落盘 / Terminal / 出队 / 活跃槽）之前拒绝。
    #[tokio::test]
    async fn new_task_launch_rejects_invalid_input_before_side_effects() {
        let launch = |kind: &str, permission: &str, task: &str| {
            new_task_launch(
                "/tmp".to_string(),
                kind.to_string(),
                permission.to_string(),
                task.to_string(),
                None,
                None,
                None,
            )
        };
        let err = launch("nope", "yolo", "task").await.unwrap_err();
        assert!(err.contains("unknown agent kind"));
        // 前端 ask 模式未选择时不应提交；后端仍兜底拒绝空/未知权限。
        let err = launch("claude", "ask", "task").await.unwrap_err();
        assert!(err.contains("permission"));
        let err = launch("claude", "yolo", "bad\0task").await.unwrap_err();
        assert!(err.contains("NUL"));
    }

    fn title_request(
        token: &str,
        agent_kind: &str,
        session_id: &str,
    ) -> HistorySessionTitleRequest {
        HistorySessionTitleRequest {
            token: token.to_string(),
            agent_kind: agent_kind.to_string(),
            session_id: session_id.to_string(),
        }
    }

    #[test]
    fn history_title_batch_validates_deduplicates_and_caches() {
        let now = Instant::now();
        let mut cache = HashMap::new();
        let mut calls = 0;
        let output = resolve_history_session_titles_with(
            vec![
                title_request("a", "codex", "s1"),
                title_request("a", "codex", "s1"), // duplicate token
                title_request("b", "codex", "s1"), // same lookup, distinct token
                title_request("bad", "unknown", "s2"),
                title_request("empty", "cursor", "  "),
            ],
            now,
            &mut cache,
            |_, _| {
                calls += 1;
                Some("  Session title  ".to_string())
            },
        );
        assert_eq!(calls, 1);
        assert_eq!(
            output,
            vec![
                HistorySessionTitleResult {
                    token: "a".into(),
                    title: "Session title".into(),
                },
                HistorySessionTitleResult {
                    token: "b".into(),
                    title: "Session title".into(),
                },
            ]
        );

        let cached = resolve_history_session_titles_with(
            vec![title_request("c", "codex", "s1")],
            now + Duration::from_secs(1),
            &mut cache,
            |_, _| panic!("fresh cache entry should be reused"),
        );
        assert_eq!(cached[0].title, "Session title");
    }

    #[test]
    fn missing_history_title_is_retried_after_short_ttl() {
        let now = Instant::now();
        let mut cache = HashMap::new();
        let missing = resolve_history_session_titles_with(
            vec![title_request("a", "cursor", "s")],
            now,
            &mut cache,
            |_, _| None,
        );
        assert!(missing.is_empty());

        let found = resolve_history_session_titles_with(
            vec![title_request("a", "cursor", "s")],
            now + HISTORY_TITLE_MISS_TTL + Duration::from_secs(1),
            &mut cache,
            |_, _| Some("Now available".into()),
        );
        assert_eq!(found[0].title, "Now available");
    }

    #[test]
    fn popup_history_target_prefers_native_session_then_project_scoped_mcp() {
        assert_eq!(
            history_open_target("/p", Some("codex"), Some("session"), Some("instance")),
            Some(crate::gui_host::HistoryOpenTarget::Agent {
                agent_kind: "codex".into(),
                session_id: "session".into(),
            })
        );
        assert_eq!(
            history_open_target("/p", None, Some("orphan"), Some("instance")),
            Some(crate::gui_host::HistoryOpenTarget::Mcp {
                project: "/p".into(),
                instance_id: "instance".into(),
            })
        );
        assert!(history_open_target("", None, None, Some("instance")).is_none());
        assert!(history_open_target("/p", Some("codex"), None, None).is_none());
    }
}
