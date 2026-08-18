//! IM `/todo`, `/todo-rm`, and `/todo-auto` inbound commands and card flows.
//!
//! 流程：`/todo`（无参）→ 复用跨渠道单选卡选项目 → 发「待办管理卡」；`/todo <text>`
//! 先选项目再追加。`/todo-rm` 同样先选项目，再复用单选卡逐条删除（就地刷新）。
//! `/todo-auto` 镜像 `/todo`：切换卡每条待办一个「切换」按钮（已自动的带 ⚡ 徽标），点击翻转
//! 自动执行标记并就地刷新；飞书在同卡追加自动待办输入框，钉钉另发新增卡（空项目只发新增卡），
//! TG/Slack 提示 `/todo-auto <text>`；该带文本形式先选项目再新增一条自动执行待办。旧的
//! `/todo <n>`、`/todo <n> <text>`、`/todo-rm <n>`、`/todo-auto <n> [text]` Agent 编号形式
//! 继续兼容，但不再作为主入口。管理卡新增入口按渠道分化：飞书代码卡自带
//! 输入框（表单提交）；钉钉复用**提问卡模板**（自带 `allow_input` 输入框，无需新注册模板，
//! 提交后复位表单以便连续新增）；TG/Slack 无可靠卡内输入 → 文本列表 + `/todo <text>` 提示。
//! 项目候选 = 工作中 Agent 项目 + 空闲 Agent 项目 + 置顶/最近 workspace + 已有待办的项目；
//! 待办读写直达 `todos.json`。

use super::*;

/// 管理卡台账负载（`PickerEntry::payload` JSON）：重建卡片只需要项目 key。
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct TodoManagePayload {
    project: String,
    #[serde(default)]
    auto: bool,
}

fn manage_payload(picker: &PickerEntry) -> Option<TodoManagePayload> {
    serde_json::from_str(picker.payload.as_deref()?).ok()
}

// ===== 命令入口 =====

pub(super) async fn handle_todo_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    content: Option<String>,
    config: &AppConfig,
    lang: Lang,
) {
    activate_channel_on_action(state, channel_id, config, lang).await;
    match (sel, content) {
        (Some(n), content) => {
            let Some(project) = project_by_seq(state, channel_id, n, config, lang).await else {
                return;
            };
            match content {
                // `/todo <n> <text>`：直接追加一条（四渠道通用的新增形式）。
                Some(text) => {
                    let Ok(entry) = crate::todos::add(&project, &text) else {
                        return;
                    };
                    let count = crate::todos::index_of(&project, &entry.id).unwrap_or(0);
                    let msg = crate::i18n::tr(lang, "todoIm.added")
                        .replace("{project}", &crate::project::display_name(&project))
                        .replace("{n}", &count.to_string());
                    let _ = reply_channel_text(channel_id, config, &msg).await;
                }
                // `/todo <n>`：直达该项目的管理卡。
                None => send_todo_manage(state, channel_id, config, &project, lang).await,
            }
        }
        // `/todo` 或 `/todo <text>`：选项目；有文本时选中后直接追加。
        (None, content) => {
            send_todo_project_picker(
                state,
                channel_id,
                config,
                PickerKind::Todo,
                content,
                true,
                lang,
            )
            .await;
        }
    }
}

pub(super) async fn handle_todo_rm_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    config: &AppConfig,
    lang: Lang,
) {
    activate_channel_on_action(state, channel_id, config, lang).await;
    match sel {
        // `/todo-rm <n>`：直达该项目的逐条删除卡（10 条上限视图，与选卡变身路径一致）。
        Some(n) => {
            let Some(project) = project_by_seq(state, channel_id, n, config, lang).await else {
                return;
            };
            let name = crate::project::display_name(&project);
            let Some((view, ids)) = todo_rm_view(&project, lang) else {
                let msg = crate::i18n::tr(lang, "todoIm.rmEmpty").replace("{project}", &name);
                let _ = reply_channel_text(channel_id, config, &msg).await;
                return;
            };
            let sent = send_todo_entry_card(
                state,
                channel_id,
                config,
                PickerKind::TodoRmEntry,
                &view,
                ids,
                project,
            )
            .await;
            if !sent {
                let _ = reply_channel_text(channel_id, config, &view_as_text(&view)).await;
            }
        }
        // `/todo-rm`（无参）：选项目单选卡。
        None => {
            send_todo_project_picker(
                state,
                channel_id,
                config,
                PickerKind::TodoRm,
                None,
                true,
                lang,
            )
            .await;
        }
    }
}

/// `/todo-auto`（第 17 轮定案）：语法镜像 `/todo`——`<n> <text>` 新增自动待办；`<n>` / 无参 →
/// 切换卡（每条待办一个「切换」按钮，已自动的带 ⚡ 徽标，点击即开/关并就地刷新）。
pub(super) async fn handle_todo_auto_cmd(
    state: &Arc<ServerState>,
    channel_id: &str,
    sel: Option<u64>,
    content: Option<String>,
    config: &AppConfig,
    lang: Lang,
) {
    activate_channel_on_action(state, channel_id, config, lang).await;
    match (sel, content) {
        (Some(n), content) => {
            let Some(project) = project_by_seq(state, channel_id, n, config, lang).await else {
                return;
            };
            match content {
                // `/todo-auto <n> <text>`：直接追加一条自动执行待办。
                Some(text) => {
                    let Ok(entry) = crate::todos::add_auto(&project, &text) else {
                        return;
                    };
                    let count = crate::todos::index_of(&project, &entry.id).unwrap_or(0);
                    let msg = crate::i18n::tr(lang, "todoIm.addedAuto")
                        .replace("{project}", &crate::project::display_name(&project))
                        .replace("{n}", &count.to_string());
                    let _ = reply_channel_text(channel_id, config, &msg).await;
                }
                // `/todo-auto <n>`：兼容直达该项目的切换 + 新增控制面。
                None => send_todo_auto_controls(state, channel_id, config, &project, lang).await,
            }
        }
        // `/todo-auto` 或 `/todo-auto <text>`：选项目；有文本时选中后直接新增自动待办。
        (None, content) => {
            send_todo_project_picker(
                state,
                channel_id,
                config,
                PickerKind::TodoAuto,
                content,
                true,
                lang,
            )
            .await;
        }
    }
}

/// `/todo <n>` 系：seq → 项目 key（cwd 的 git 根）。找不到 / 无 cwd → 回提示并返回 None。
async fn project_by_seq(
    state: &Arc<ServerState>,
    channel_id: &str,
    n: u64,
    config: &AppConfig,
    lang: Lang,
) -> Option<String> {
    let prefix = crate::autochannel::cmd_prefix(channel_id);
    let snapshot = state.agents.snapshot();
    let Some(rec) = crate::autochannel::find_by_seq(&snapshot, n) else {
        let text = crate::i18n::tr(lang, "todoIm.notFound")
            .replace("{n}", &n.to_string())
            .replace("{p}", prefix);
        let _ = reply_channel_text(channel_id, config, &text).await;
        return None;
    };
    let key = project_of_record(Some(rec));
    if key.is_none() {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "todoIm.noProject"),
        )
        .await;
    }
    key
}

/// 快照记录 → 项目 key（cwd 的 git 根）；无 cwd → None。
pub(super) fn project_of_record(rec: Option<&serde_json::Value>) -> Option<String> {
    let cwd = rec?
        .get("cwd")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    let key = crate::project::detect_from(std::path::Path::new(cwd));
    (!key.is_empty()).then_some(key)
}

// ===== 项目选择 =====

#[derive(Clone)]
struct TodoProjectCandidate {
    key: String,
    pinned: bool,
    last_used_at: u64,
    has_workspace: bool,
    /// 0 = working, 1 = idle, 2 = no live Agent.
    activity_rank: u8,
    todo_count: usize,
}

fn todo_project_candidates(
    snapshot: &serde_json::Value,
    workspaces: Vec<crate::agents::workspaces::Workspace>,
    todos: &std::collections::HashMap<String, Vec<crate::todos::TodoEntry>>,
) -> Vec<TodoProjectCandidate> {
    let mut by_key: std::collections::HashMap<String, TodoProjectCandidate> =
        std::collections::HashMap::new();

    for workspace in workspaces.into_iter().filter(|workspace| !workspace.hidden) {
        let key = crate::project::detect_from(std::path::Path::new(&workspace.path));
        if key.is_empty() {
            continue;
        }
        let entry = by_key
            .entry(key.clone())
            .or_insert_with(|| TodoProjectCandidate {
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

    for (key, entries) in todos {
        let entry = by_key
            .entry(key.clone())
            .or_insert_with(|| TodoProjectCandidate {
                key: key.clone(),
                pinned: false,
                last_used_at: 0,
                has_workspace: false,
                activity_rank: 2,
                todo_count: 0,
            });
        entry.todo_count = entries.len();
    }

    for rec in snapshot.as_array().map(Vec::as_slice).unwrap_or(&[]) {
        let rank = match rec.get("state").and_then(|value| value.as_str()) {
            Some("working") => 0,
            Some("idle") => 1,
            _ => continue,
        };
        let Some(key) = project_of_record(Some(rec)) else {
            continue;
        };
        let entry = by_key
            .entry(key.clone())
            .or_insert_with(|| TodoProjectCandidate {
                key,
                pinned: false,
                last_used_at: 0,
                has_workspace: false,
                activity_rank: rank,
                todo_count: 0,
            });
        entry.activity_rank = entry.activity_rank.min(rank);
    }

    let mut candidates: Vec<_> = by_key.into_values().collect();
    candidates.sort_by(|a, b| {
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
    });
    candidates
}

fn todo_project_options(
    candidates: Vec<TodoProjectCandidate>,
    recent_only: bool,
    lang: Lang,
) -> Vec<crate::select::SelectOption> {
    let total = candidates.len();
    let visible = if recent_only { total.min(5) } else { total };
    let home = crate::paths::home().to_string_lossy().to_string();
    let mut options: Vec<_> = candidates
        .into_iter()
        .take(visible)
        .map(|candidate| {
            let parent = std::path::Path::new(&candidate.key)
                .parent()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default();
            let parent = parent
                .strip_prefix(&home)
                .map(|rest| format!("~{rest}"))
                .unwrap_or(parent);
            crate::select::SelectOption {
                id: candidate.key.clone(),
                dot: match candidate.activity_rank {
                    0 => Some(crate::select::SelectDot::Working),
                    1 => Some(crate::select::SelectDot::Idle),
                    _ => None,
                },
                seq: None,
                primary: if candidate.pinned {
                    format!("★ {}", crate::project::display_name(&candidate.key))
                } else {
                    crate::project::display_name(&candidate.key)
                },
                badge: (candidate.todo_count > 0).then(|| {
                    crate::i18n::tr(lang, "todoIm.projectTodoBadge")
                        .replace("{n}", &candidate.todo_count.to_string())
                }),
                elapsed: None,
                secondary: (!parent.is_empty()).then_some(parent),
            }
        })
        .collect();
    if recent_only && total > visible {
        options.push(crate::select::SelectOption {
            id: crate::select::MORE_OPTION_ID.to_string(),
            dot: None,
            seq: None,
            primary: match lang {
                Lang::Zh => "显示更多项目",
                Lang::En => "Show more projects",
            }
            .into(),
            badge: None,
            elapsed: None,
            secondary: Some(match lang {
                Lang::Zh => format!("还有 {} 个", total - visible),
                Lang::En => format!("{} more", total - visible),
            }),
        });
    }
    options
}

fn todo_picker_title(kind: PickerKind, lang: Lang) -> String {
    match kind {
        PickerKind::Todo => crate::select::title_todo(lang),
        PickerKind::TodoRm => crate::select::title_todo_rm(lang),
        PickerKind::TodoAuto => crate::select::title_todo_auto(lang),
        _ => String::new(),
    }
}

async fn load_todo_project_options(
    state: &Arc<ServerState>,
    recent_only: bool,
    lang: Lang,
) -> Vec<crate::select::SelectOption> {
    let snapshot = state.agents.snapshot();
    let (workspaces, todos) = tokio::task::spawn_blocking(move || {
        let workspaces = if recent_only {
            crate::agents::workspaces::refresh()
        } else {
            crate::agents::workspaces::list()
        };
        (workspaces, crate::todos::all())
    })
    .await
    .unwrap_or_default();
    todo_project_options(
        todo_project_candidates(&snapshot, workspaces, &todos),
        recent_only,
        lang,
    )
}

async fn send_todo_project_picker(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    kind: PickerKind,
    content: Option<String>,
    recent_only: bool,
    lang: Lang,
) {
    let options = load_todo_project_options(state, recent_only, lang).await;
    if options.is_empty() {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "todoIm.noProjects"),
        )
        .await;
        return;
    }
    let sent = send_agent_picker(
        state,
        channel_id,
        config,
        kind,
        todo_picker_title(kind, lang),
        options,
        content,
        lang,
    )
    .await;
    if !sent {
        let _ = reply_channel_text(
            channel_id,
            config,
            crate::i18n::tr(lang, "todoIm.projectPickerFailed"),
        )
        .await;
    }
}

pub(super) async fn select_pick_todo_more(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    picker: &PickerEntry,
    config: &AppConfig,
    lang: Lang,
    ack: Option<crate::confirm::transport::FsAck>,
) {
    let title = todo_picker_title(picker.kind, lang);
    let label = match lang {
        Lang::Zh => "显示更多项目",
        Lang::En => "Show more projects",
    };
    if channel_id == "feishu" {
        if let Some(ack) = ack {
            let card = crate::feishu::card::build_select_final_card(&title, label);
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        }
    } else if channel_id == "dingding" {
        dd_finalize_select_card(config, mid, label).await;
    } else {
        finalize_select_card_edit(channel_id, config, mid, &title, label).await;
    }
    remove_picker(state, channel_id, mid);
    state.select.route_refresh.notify_one();
    send_todo_project_picker(
        state,
        channel_id,
        config,
        picker.kind,
        picker.payload.clone(),
        false,
        lang,
    )
    .await;
}

// ===== 内容渲染 =====

/// 待办列表主体（markdown / 纯文本通用）：`1. text` 逐行；空 → 「（暂无待办）」。
fn todo_lines(entries: &[crate::todos::TodoEntry], lang: Lang) -> String {
    if entries.is_empty() {
        return crate::i18n::tr(lang, "todoIm.empty").to_string();
    }
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| format!("{}. {}", i + 1, e.text))
        .collect::<Vec<_>>()
        .join("\n")
}

fn manage_title(project: &str, lang: Lang) -> String {
    crate::i18n::tr(lang, "todoIm.listTitle")
        .replace("{project}", &crate::project::display_name(project))
}

fn rm_hint(prefix: &str, lang: Lang) -> String {
    crate::i18n::tr(lang, "todoIm.rmHint").replace("{p}", prefix)
}

/// TG/Slack（及卡片发送失败兜底）的文本列表：标题 + 列表 + 新增/删除提示。
fn todo_list_text(
    project: &str,
    entries: &[crate::todos::TodoEntry],
    prefix: &str,
    lang: Lang,
) -> String {
    let mut out = manage_title(project, lang);
    out.push('\n');
    out.push_str(&todo_lines(entries, lang));
    out.push_str("\n\n");
    out.push_str(&crate::i18n::tr(lang, "todoIm.addHint").replace("{p}", prefix));
    out
}

/// 组装飞书管理卡（列表 + 灰色删除提示 + 输入框表单）。
fn fs_manage_card(project: &str, lang: Lang) -> serde_json::Value {
    let entries = crate::todos::list(project);
    let prefix = crate::autochannel::cmd_prefix("feishu");
    let body = format!(
        "{}\n\n<font color='grey'>{}</font>",
        todo_lines(&entries, lang),
        rm_hint(prefix, lang)
    );
    crate::feishu::card::build_todo_manage_card(
        &manage_title(project, lang),
        &body,
        crate::i18n::tr(lang, "todoIm.cardInputPlaceholder"),
        crate::i18n::tr(lang, "todoIm.cardAddButton"),
    )
}

/// 钉钉管理卡正文 markdown（提问卡模板的 `markdown` 变量）。
fn dd_manage_markdown(project: &str, lang: Lang) -> String {
    let entries = crate::todos::list(project);
    let prefix = crate::autochannel::cmd_prefix("dingding");
    format!(
        "{}\n\n{}",
        todo_lines(&entries, lang),
        rm_hint(prefix, lang)
    )
}

// ===== 管理卡发送 / 台账 =====

/// 发送一张待办管理卡（飞书代码卡 / 钉钉提问卡模板 / TG·Slack 文本列表）。
pub(super) async fn send_todo_manage(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    project: &str,
    lang: Lang,
) {
    let prefix = crate::autochannel::cmd_prefix(channel_id);
    match channel_id {
        "feishu" => {
            let card = fs_manage_card(project, lang);
            let mid = match crate::feishu::client::FeishuClient::new(&config.channels.feishu) {
                Ok(client) => client.send_card(&card).await.ok(),
                Err(_) => None,
            };
            match mid {
                Some(mid) => register_todo_manage(state, channel_id, &mid, project, lang),
                None => {
                    let entries = crate::todos::list(project);
                    let _ = reply_channel_text(
                        channel_id,
                        config,
                        &todo_list_text(project, &entries, prefix, lang),
                    )
                    .await;
                }
            }
        }
        "dingding" => {
            // 复用提问卡模板：正文 markdown 放列表、options 置空、`allow_input` 开输入框。
            let otid = format!("todo-{}", uuid::Uuid::new_v4());
            let map = crate::dingtalk::card::build_card_param_map(
                &manage_title(project, lang),
                &dd_manage_markdown(project, lang),
                &[],
                false,
                false,
                "",
            );
            let private = crate::dingtalk::card::build_card_private_map();
            let ok = match crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) {
                Ok(client) => client
                    .create_and_deliver_card(
                        &otid,
                        crate::channels::dingding::effective_template_id(&config.channels.dingding),
                        map,
                        private,
                    )
                    .await
                    .is_ok(),
                Err(_) => false,
            };
            if ok {
                register_todo_manage(state, channel_id, &otid, project, lang);
            } else {
                let entries = crate::todos::list(project);
                let _ = reply_channel_text(
                    channel_id,
                    config,
                    &todo_list_text(project, &entries, prefix, lang),
                )
                .await;
            }
        }
        // TG/Slack：无可靠卡内输入 → 文本列表 + `/todo <text>` 提示（spec D8）。
        _ => {
            let entries = crate::todos::list(project);
            let _ = reply_channel_text(
                channel_id,
                config,
                &todo_list_text(project, &entries, prefix, lang),
            )
            .await;
        }
    }
}

fn register_todo_manage(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    project: &str,
    lang: Lang,
) {
    let payload = serde_json::to_string(&TodoManagePayload {
        project: project.to_string(),
        auto: false,
    })
    .ok();
    register_picker(
        state,
        PickerEntry {
            channel: channel_id.to_string(),
            message_id: mid.to_string(),
            kind: PickerKind::TodoManage,
            title: manage_title(project, lang),
            options: Vec::new(),
            payload,
            created_at: now_secs(),
            posted_ms: now_ms(),
        },
    );
    state.select.route_refresh.notify_one();
}

/// 把一张已发出的卡的台账就地改造（飞书变身 / 钉钉同模板刷新后共用）。
/// `title`＝变身后卡片的标题快照（关停定格终态卡时复用）。
fn morph_picker(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    kind: PickerKind,
    title: String,
    options: Vec<String>,
    payload: Option<String>,
) {
    if let Some(p) = state
        .select
        .pickers
        .lock()
        .unwrap()
        .iter_mut()
        .find(|p| p.channel == channel_id && p.message_id == mid)
    {
        p.kind = kind;
        p.title = title;
        p.options = options;
        p.payload = payload;
    }
}

// ===== 选项目卡点选（Todo / TodoRm）=====

fn project_picked_label(project: &str, lang: Lang) -> String {
    crate::i18n::tr(lang, "todoIm.projectPicked")
        .replace("{project}", &crate::project::display_name(project))
}

fn add_from_picker(project: &str, content: &str, auto: bool, lang: Lang) -> Option<String> {
    let added = if auto {
        crate::todos::add_auto(project, content)
    } else {
        crate::todos::add(project, content)
    }
    .ok()?;
    let count = crate::todos::index_of(project, &added.id).unwrap_or(0);
    if count == 0 {
        return None;
    }
    let key = if added.auto {
        "todoIm.addedAuto"
    } else {
        "todoIm.added"
    };
    Some(
        crate::i18n::tr(lang, key)
            .replace("{project}", &crate::project::display_name(project))
            .replace("{n}", &count.to_string()),
    )
}

/// 飞书 `/todo` 选卡点选：本卡就地变身为该项目的待办管理卡（台账同步改 kind）。
pub(super) async fn fs_select_pick_todo(
    state: &Arc<ServerState>,
    mid: &str,
    project: &str,
    content: Option<&str>,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    if let Some(content) = content {
        let label = add_from_picker(project, content, false, lang)
            .unwrap_or_else(|| crate::i18n::tr(lang, "todoIm.addFailed").to_string());
        let card =
            crate::feishu::card::build_select_final_card(&crate::select::title_todo(lang), &label);
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, "feishu", mid);
        return;
    }
    let card = fs_manage_card(project, lang);
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    let title = manage_title(project, lang);
    let payload = serde_json::to_string(&TodoManagePayload {
        project: project.to_string(),
        auto: false,
    })
    .ok();
    morph_picker(
        state,
        "feishu",
        mid,
        PickerKind::TodoManage,
        title,
        Vec::new(),
        payload,
    );
}

/// 钉钉 `/todo` 选卡点选：钉钉不能跨模板变身 → 单选卡定格项目名，另发管理卡。
pub(super) async fn dd_select_pick_todo(
    state: &Arc<ServerState>,
    otid: &str,
    project: &str,
    content: Option<&str>,
    config: &AppConfig,
    lang: Lang,
) {
    let label = content
        .map(|content| {
            add_from_picker(project, content, false, lang)
                .unwrap_or_else(|| crate::i18n::tr(lang, "todoIm.addFailed").to_string())
        })
        .unwrap_or_else(|| project_picked_label(project, lang));
    dd_finalize_select_card(config, otid, &label).await;
    remove_picker(state, "dingding", otid);
    if content.is_none() {
        send_todo_manage(state, "dingding", config, project, lang).await;
    }
}

/// TG/Slack `/todo` 选卡点选：定格项目名 + 回文本列表（无卡内输入形态）。
pub(super) async fn select_pick_todo_text(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    project: &str,
    content: Option<&str>,
    config: &AppConfig,
    lang: Lang,
) {
    let label = content
        .map(|content| {
            add_from_picker(project, content, false, lang)
                .unwrap_or_else(|| crate::i18n::tr(lang, "todoIm.addFailed").to_string())
        })
        .unwrap_or_else(|| project_picked_label(project, lang));
    finalize_select_card_edit(
        channel_id,
        config,
        mid,
        &crate::select::title_todo(lang),
        &label,
    )
    .await;
    remove_picker(state, channel_id, mid);
    if content.is_none() {
        send_todo_manage(state, channel_id, config, project, lang).await;
    }
}

/// 组装某项目的逐条删除卡视图；返回 `(view, 条目 id 集)`。空队列 → None。
/// 只列前 `MAX_OPTION_TODOS` 条（第 14 轮定案，渠道选项数硬限制），超出时以待办版溢出提示
/// 顶替通用截断说明（`truncated_note` 槽位，四渠道既有渲染路径复用）。
fn todo_rm_view(project: &str, lang: Lang) -> Option<(crate::select::SelectView, Vec<String>)> {
    let mut entries = crate::todos::list(project);
    if entries.is_empty() {
        return None;
    }
    let total = entries.len();
    entries.truncate(crate::todos::MAX_OPTION_TODOS);
    let name = crate::project::display_name(project);
    let mut view = crate::select::build_view(
        crate::select::title_todo_rm_entries(&name, lang),
        crate::select::todo_rm_options(&entries),
        crate::select::SelectAction::TodoRmEntry,
        lang,
    );
    view.truncated_note = crate::todos::overflow_note(total, lang);
    let ids = view.options.iter().map(|o| o.id.clone()).collect();
    Some((view, ids))
}

/// 组装某项目的「切换自动执行」卡视图（第 17 轮定案）；同删除卡的 10 条上限与溢出提示，
/// 已自动的条目带 ⚡ 徽标。空队列 → None。
fn todo_auto_view(project: &str, lang: Lang) -> Option<(crate::select::SelectView, Vec<String>)> {
    let mut entries = crate::todos::list(project);
    if entries.is_empty() {
        return None;
    }
    let total = entries.len();
    entries.truncate(crate::todos::MAX_OPTION_TODOS);
    let name = crate::project::display_name(project);
    let mut view = crate::select::build_view(
        crate::select::title_todo_auto_entries(&name, lang),
        crate::select::todo_auto_options(&entries, lang),
        crate::select::SelectAction::TodoAutoEntry,
        lang,
    );
    view.truncated_note = crate::todos::overflow_note(total, lang);
    let ids = view.options.iter().map(|o| o.id.clone()).collect();
    Some((view, ids))
}

fn todo_auto_view_or_empty(project: &str, lang: Lang) -> (crate::select::SelectView, Vec<String>) {
    todo_auto_view(project, lang).unwrap_or_else(|| {
        let name = crate::project::display_name(project);
        (
            crate::select::build_view(
                crate::select::title_todo_auto_entries(&name, lang),
                Vec::new(),
                crate::select::SelectAction::TodoAutoEntry,
                lang,
            ),
            Vec::new(),
        )
    })
}

fn fs_todo_auto_card(view: &crate::select::SelectView, lang: Lang) -> serde_json::Value {
    crate::feishu::card::build_todo_auto_card(
        view,
        crate::i18n::tr(lang, "select.todoAutoEmptyCard"),
        crate::i18n::tr(lang, "todoIm.autoCardInputPlaceholder"),
        crate::i18n::tr(lang, "todoIm.autoCardAddButton"),
    )
}

fn todo_auto_add_title(project: &str, lang: Lang) -> String {
    crate::i18n::tr(lang, "todoIm.autoAddTitle")
        .replace("{project}", &crate::project::display_name(project))
}

fn todo_auto_add_hint(channel_id: &str, lang: Lang) -> String {
    crate::i18n::tr(lang, "todoIm.autoAddHint")
        .replace("{p}", crate::autochannel::cmd_prefix(channel_id))
}

async fn send_dd_todo_auto_add_card(
    state: &Arc<ServerState>,
    config: &AppConfig,
    project: &str,
    lang: Lang,
) -> bool {
    let otid = format!("todo-auto-add-{}", uuid::Uuid::new_v4());
    let title = todo_auto_add_title(project, lang);
    let map = crate::dingtalk::card::build_card_param_map(
        &title,
        crate::i18n::tr(lang, "todoIm.autoAddCardHint"),
        &[],
        false,
        false,
        "",
    );
    let private = crate::dingtalk::card::build_card_private_map();
    let sent = match crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) {
        Ok(client) => client
            .create_and_deliver_card(
                &otid,
                crate::channels::dingding::effective_template_id(&config.channels.dingding),
                map,
                private,
            )
            .await
            .is_ok(),
        Err(_) => false,
    };
    if !sent {
        return false;
    }
    let payload = serde_json::to_string(&TodoManagePayload {
        project: project.to_string(),
        auto: true,
    })
    .ok();
    register_picker(
        state,
        PickerEntry {
            channel: "dingding".to_string(),
            message_id: otid,
            kind: PickerKind::TodoManage,
            title,
            options: Vec::new(),
            payload,
            created_at: now_secs(),
            posted_ms: now_ms(),
        },
    );
    state.select.route_refresh.notify_one();
    true
}

/// 直接发送一张预构建的待办条目卡（删除 / 切换）并登记台账：直达路径用（不经
/// `send_agent_picker` 的通用 20 条截断，保持 10 条上限视图一致）。发送失败返回 false。
async fn send_todo_entry_card(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    kind: PickerKind,
    view: &crate::select::SelectView,
    ids: Vec<String>,
    project: String,
) -> bool {
    let Some(mid) = send_select_card(channel_id, config, view).await else {
        return false;
    };
    register_picker(
        state,
        PickerEntry {
            channel: channel_id.to_string(),
            message_id: mid,
            kind,
            title: view.title.clone(),
            options: ids,
            payload: Some(project),
            created_at: now_secs(),
            posted_ms: now_ms(),
        },
    );
    state.select.route_refresh.notify_one();
    true
}

/// 发送 `/todo-auto` 项目控制面：飞书同卡“切换 + 新增”；钉钉有条目时发切换卡，
/// 并始终另发新增卡；TG/Slack 保留切换卡并用文本提示新增命令。空项目仍提供新增入口。
async fn send_todo_auto_controls(
    state: &Arc<ServerState>,
    channel_id: &str,
    config: &AppConfig,
    project: &str,
    lang: Lang,
) {
    let populated = todo_auto_view(project, lang);
    match channel_id {
        "feishu" => {
            let (view, ids) = todo_auto_view_or_empty(project, lang);
            let card = fs_todo_auto_card(&view, lang);
            let mid = match crate::feishu::client::FeishuClient::new(&config.channels.feishu) {
                Ok(client) => client.send_card(&card).await.ok(),
                Err(_) => None,
            };
            if let Some(mid) = mid {
                register_picker(
                    state,
                    PickerEntry {
                        channel: channel_id.to_string(),
                        message_id: mid,
                        kind: PickerKind::TodoAutoEntry,
                        title: view.title,
                        options: ids,
                        payload: Some(project.to_string()),
                        created_at: now_secs(),
                        posted_ms: now_ms(),
                    },
                );
                state.select.route_refresh.notify_one();
            } else {
                let mut text = view_as_text(&view);
                text.push_str("\n\n");
                text.push_str(&todo_auto_add_hint(channel_id, lang));
                let _ = reply_channel_text(channel_id, config, &text).await;
            }
        }
        "dingding" => {
            if let Some((view, ids)) = populated {
                let sent = send_todo_entry_card(
                    state,
                    channel_id,
                    config,
                    PickerKind::TodoAutoEntry,
                    &view,
                    ids,
                    project.to_string(),
                )
                .await;
                if !sent {
                    let _ = reply_channel_text(channel_id, config, &view_as_text(&view)).await;
                }
            }
            if !send_dd_todo_auto_add_card(state, config, project, lang).await {
                let _ =
                    reply_channel_text(channel_id, config, &todo_auto_add_hint(channel_id, lang))
                        .await;
            }
        }
        _ => {
            let mut text = String::new();
            if let Some((view, ids)) = populated {
                if !send_todo_entry_card(
                    state,
                    channel_id,
                    config,
                    PickerKind::TodoAutoEntry,
                    &view,
                    ids,
                    project.to_string(),
                )
                .await
                {
                    text.push_str(&view_as_text(&view));
                    text.push_str("\n\n");
                }
            } else {
                text.push_str(crate::i18n::tr(lang, "select.todoAutoEmptyCard"));
                text.push_str("\n\n");
            }
            text.push_str(&todo_auto_add_hint(channel_id, lang));
            let _ = reply_channel_text(channel_id, config, &text).await;
        }
    }
}

/// 卡片发送失败（渠道不支持）时的文本兜底：标题 + 编号列表（含 ⚡ 徽标）+ 溢出提示。
fn view_as_text(view: &crate::select::SelectView) -> String {
    let mut out = view.title.clone();
    for o in &view.options {
        out.push('\n');
        if let Some(n) = o.seq {
            out.push_str(&format!("{}. ", n));
        }
        out.push_str(&o.primary);
        if let Some(badge) = &o.badge {
            out.push(' ');
            out.push_str(badge);
        }
    }
    if let Some(note) = &view.truncated_note {
        out.push('\n');
        out.push_str(note);
    }
    out
}

/// 飞书 `/todo-rm` 选卡点选：本卡就地变身为逐条删除卡（空队列 → 定格提示）。
pub(super) async fn fs_select_pick_todo_rm(
    state: &Arc<ServerState>,
    mid: &str,
    project: &str,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    match todo_rm_view(project, lang) {
        Some((view, ids)) => {
            let card = crate::feishu::card::build_select_card(&view);
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
            morph_picker(
                state,
                "feishu",
                mid,
                PickerKind::TodoRmEntry,
                view.title.clone(),
                ids,
                Some(project.to_string()),
            );
        }
        None => {
            let msg = crate::i18n::tr(lang, "todoIm.rmEmpty")
                .replace("{project}", &crate::project::display_name(project));
            let card = crate::feishu::card::build_select_final_card(
                &crate::select::title_todo_rm(lang),
                &msg,
            );
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
            remove_picker(state, "feishu", mid);
        }
    }
}

/// 钉钉 `/todo-rm` 选卡点选：同模板 → 本卡经 OpenAPI 就地刷新为逐条删除卡。
pub(super) async fn dd_select_pick_todo_rm(
    state: &Arc<ServerState>,
    otid: &str,
    project: &str,
    config: &AppConfig,
    lang: Lang,
) {
    match todo_rm_view(project, lang) {
        Some((view, ids)) => {
            if let Ok(client) =
                crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
            {
                let map = crate::dingtalk::select::build_select_param_map(&view, lang);
                if let Err(err) = client
                    .update_card_private(otid, map, serde_json::json!({}))
                    .await
                {
                    log(&format!("todo: refresh dingtalk rm card failed: {}", err));
                }
            }
            morph_picker(
                state,
                "dingding",
                otid,
                PickerKind::TodoRmEntry,
                view.title.clone(),
                ids,
                Some(project.to_string()),
            );
        }
        None => {
            let msg = crate::i18n::tr(lang, "todoIm.rmEmpty")
                .replace("{project}", &crate::project::display_name(project));
            dd_finalize_select_card(config, otid, &msg).await;
            remove_picker(state, "dingding", otid);
        }
    }
}

/// TG/Slack `/todo-rm` 选卡点选：本卡就地编辑为逐条删除卡。
pub(super) async fn select_pick_todo_rm_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    project: &str,
    config: &AppConfig,
    lang: Lang,
) {
    match todo_rm_view(project, lang) {
        Some((view, ids)) => {
            refresh_select_card_edit(channel_id, config, mid, &view, lang).await;
            morph_picker(
                state,
                channel_id,
                mid,
                PickerKind::TodoRmEntry,
                view.title.clone(),
                ids,
                Some(project.to_string()),
            );
        }
        None => {
            let msg = crate::i18n::tr(lang, "todoIm.rmEmpty")
                .replace("{project}", &crate::project::display_name(project));
            finalize_select_card_edit(
                channel_id,
                config,
                mid,
                &crate::select::title_todo_rm(lang),
                &msg,
            )
            .await;
            remove_picker(state, channel_id, mid);
        }
    }
}

// ===== 选项目卡点选（TodoAuto，第 17 轮定案）=====

/// 飞书 `/todo-auto` 选卡点选：本卡就地变身为切换卡（空队列 → 定格提示）。
pub(super) async fn fs_select_pick_todo_auto(
    state: &Arc<ServerState>,
    mid: &str,
    project: &str,
    content: Option<&str>,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    if let Some(content) = content {
        let label = add_from_picker(project, content, true, lang)
            .unwrap_or_else(|| crate::i18n::tr(lang, "todoIm.addFailed").to_string());
        let card = crate::feishu::card::build_select_final_card(
            &crate::select::title_todo_auto(lang),
            &label,
        );
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        remove_picker(state, "feishu", mid);
        return;
    }
    let (view, ids) = todo_auto_view_or_empty(project, lang);
    let card = fs_todo_auto_card(&view, lang);
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    morph_picker(
        state,
        "feishu",
        mid,
        PickerKind::TodoAutoEntry,
        view.title.clone(),
        ids,
        Some(project.to_string()),
    );
}

/// 钉钉 `/todo-auto` 选卡点选：同模板 → 本卡经 OpenAPI 就地刷新为切换卡。
pub(super) async fn dd_select_pick_todo_auto(
    state: &Arc<ServerState>,
    otid: &str,
    project: &str,
    content: Option<&str>,
    config: &AppConfig,
    lang: Lang,
) {
    if let Some(content) = content {
        let label = add_from_picker(project, content, true, lang)
            .unwrap_or_else(|| crate::i18n::tr(lang, "todoIm.addFailed").to_string());
        dd_finalize_select_card(config, otid, &label).await;
        remove_picker(state, "dingding", otid);
        return;
    }
    match todo_auto_view(project, lang) {
        Some((view, ids)) => {
            dd_refresh_entry_card(config, otid, &view, lang).await;
            morph_picker(
                state,
                "dingding",
                otid,
                PickerKind::TodoAutoEntry,
                view.title.clone(),
                ids,
                Some(project.to_string()),
            );
        }
        None => {
            dd_finalize_select_card(
                config,
                otid,
                crate::i18n::tr(lang, "select.todoAutoEmptyCard"),
            )
            .await;
            remove_picker(state, "dingding", otid);
        }
    }
    if !send_dd_todo_auto_add_card(state, config, project, lang).await {
        let _ = reply_channel_text("dingding", config, &todo_auto_add_hint("dingding", lang)).await;
    }
}

/// TG/Slack `/todo-auto` 选卡点选：本卡就地编辑为切换卡。
pub(super) async fn select_pick_todo_auto_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    project: &str,
    content: Option<&str>,
    config: &AppConfig,
    lang: Lang,
) {
    if let Some(content) = content {
        let label = add_from_picker(project, content, true, lang)
            .unwrap_or_else(|| crate::i18n::tr(lang, "todoIm.addFailed").to_string());
        finalize_select_card_edit(
            channel_id,
            config,
            mid,
            &crate::select::title_todo_auto(lang),
            &label,
        )
        .await;
        remove_picker(state, channel_id, mid);
        return;
    }
    match todo_auto_view(project, lang) {
        Some((view, ids)) => {
            refresh_select_card_edit(channel_id, config, mid, &view, lang).await;
            morph_picker(
                state,
                channel_id,
                mid,
                PickerKind::TodoAutoEntry,
                view.title.clone(),
                ids,
                Some(project.to_string()),
            );
        }
        None => {
            finalize_select_card_edit(
                channel_id,
                config,
                mid,
                &crate::select::title_todo_auto(lang),
                crate::i18n::tr(lang, "select.todoAutoEmptyCard"),
            )
            .await;
            remove_picker(state, channel_id, mid);
        }
    }
    let _ = reply_channel_text(channel_id, config, &todo_auto_add_hint(channel_id, lang)).await;
}

// ===== 切换卡点「切换」（TodoAutoEntry）=====

/// 切换一条的自动标记并重算视图。返回 `(项目 key, 新视图)`；队列已空（并发清空）→ `(key, None)`。
fn auto_entry_toggle(
    picker: &PickerEntry,
    entry_id: &str,
    lang: Lang,
) -> (String, Option<(crate::select::SelectView, Vec<String>)>) {
    let project = picker.payload.clone().unwrap_or_default();
    let current = crate::todos::list(&project)
        .iter()
        .find(|e| e.id == entry_id)
        .map(|e| e.auto);
    if let Some(auto) = current {
        let _ = crate::todos::set_auto(&project, entry_id, !auto);
    }
    let view = todo_auto_view(&project, lang);
    (project, view)
}

/// 飞书切换卡点「切换」：翻转 + 就地刷新；并发清空后仍保留新增输入框。
pub(super) async fn fs_select_pick_todo_auto_entry(
    state: &Arc<ServerState>,
    mid: &str,
    entry_id: &str,
    picker: &PickerEntry,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    let (project, view) = auto_entry_toggle(picker, entry_id, lang);
    let (view, ids) = view.unwrap_or_else(|| todo_auto_view_or_empty(&project, lang));
    let card = fs_todo_auto_card(&view, lang);
    let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    morph_picker(
        state,
        "feishu",
        mid,
        PickerKind::TodoAutoEntry,
        view.title.clone(),
        ids,
        Some(project),
    );
}

/// 钉钉切换卡点「切换」：翻转 + OpenAPI 就地刷新（空 → 定格）。
pub(super) async fn dd_select_pick_todo_auto_entry(
    state: &Arc<ServerState>,
    otid: &str,
    entry_id: &str,
    picker: &PickerEntry,
    config: &AppConfig,
    lang: Lang,
) {
    let (project, view) = auto_entry_toggle(picker, entry_id, lang);
    match view {
        Some((view, ids)) => {
            dd_refresh_entry_card(config, otid, &view, lang).await;
            morph_picker(
                state,
                "dingding",
                otid,
                PickerKind::TodoAutoEntry,
                view.title.clone(),
                ids,
                Some(project),
            );
        }
        None => {
            dd_finalize_select_card(
                config,
                otid,
                crate::i18n::tr(lang, "select.todoAutoEmptyCard"),
            )
            .await;
            remove_picker(state, "dingding", otid);
        }
    }
}

/// TG/Slack 切换卡点「切换」：翻转 + 就地编辑刷新（空 → 定格）。
pub(super) async fn select_pick_todo_auto_entry_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    entry_id: &str,
    picker: &PickerEntry,
    config: &AppConfig,
    lang: Lang,
) {
    let (project, view) = auto_entry_toggle(picker, entry_id, lang);
    match view {
        Some((view, ids)) => {
            refresh_select_card_edit(channel_id, config, mid, &view, lang).await;
            morph_picker(
                state,
                channel_id,
                mid,
                PickerKind::TodoAutoEntry,
                view.title.clone(),
                ids,
                Some(project),
            );
        }
        None => {
            finalize_select_card_edit(
                channel_id,
                config,
                mid,
                &crate::select::title_todo_auto_entries(
                    &crate::project::display_name(&project),
                    lang,
                ),
                crate::i18n::tr(lang, "select.todoAutoEmptyCard"),
            )
            .await;
            remove_picker(state, channel_id, mid);
        }
    }
}

/// 钉钉：把一张同模板选择卡经 OpenAPI 刷新为给定视图（rm / auto 切换卡共用）。
async fn dd_refresh_entry_card(
    config: &AppConfig,
    otid: &str,
    view: &crate::select::SelectView,
    lang: Lang,
) {
    if let Ok(client) = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) {
        let map = crate::dingtalk::select::build_select_param_map(view, lang);
        if let Err(err) = client
            .update_card_private(otid, map, serde_json::json!({}))
            .await
        {
            log(&format!(
                "todo: refresh dingtalk entry card failed: {}",
                err
            ));
        }
    }
}

// ===== 逐条删除卡点「删除」（TodoRmEntry）=====

/// 出队一条并重算剩余视图。返回 `(项目 key, 剩余视图)`；剩余为空 → `(key, None)`。
fn rm_entry_delete(
    picker: &PickerEntry,
    entry_id: &str,
    lang: Lang,
) -> (String, Option<(crate::select::SelectView, Vec<String>)>) {
    let project = picker.payload.clone().unwrap_or_default();
    let _ = crate::todos::remove(&project, entry_id);
    let view = todo_rm_view(&project, lang);
    (project, view)
}

/// 飞书逐条删除卡点「删除」：出队 + 就地刷新（空 → 定格「已全部删除」）。
pub(super) async fn fs_select_pick_todo_rm_entry(
    state: &Arc<ServerState>,
    mid: &str,
    entry_id: &str,
    picker: &PickerEntry,
    lang: Lang,
    ack: crate::feishu::router::CardAck,
) {
    let (project, view) = rm_entry_delete(picker, entry_id, lang);
    match view {
        Some((view, ids)) => {
            let card = crate::feishu::card::build_select_card(&view);
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
            morph_picker(
                state,
                "feishu",
                mid,
                PickerKind::TodoRmEntry,
                view.title.clone(),
                ids,
                Some(project),
            );
        }
        None => {
            let card = crate::feishu::card::build_select_final_card(
                &crate::select::title_todo_rm_entries(
                    &crate::project::display_name(&project),
                    lang,
                ),
                crate::i18n::tr(lang, "select.todoRmAllDoneCard"),
            );
            let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
            remove_picker(state, "feishu", mid);
        }
    }
}

/// 钉钉逐条删除卡点「删除」：出队 + OpenAPI 就地刷新（空 → 定格）。
pub(super) async fn dd_select_pick_todo_rm_entry(
    state: &Arc<ServerState>,
    otid: &str,
    entry_id: &str,
    picker: &PickerEntry,
    config: &AppConfig,
    lang: Lang,
) {
    let (project, view) = rm_entry_delete(picker, entry_id, lang);
    match view {
        Some((view, ids)) => {
            if let Ok(client) =
                crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding)
            {
                let map = crate::dingtalk::select::build_select_param_map(&view, lang);
                if let Err(err) = client
                    .update_card_private(otid, map, serde_json::json!({}))
                    .await
                {
                    log(&format!("todo: refresh dingtalk rm card failed: {}", err));
                }
            }
            morph_picker(
                state,
                "dingding",
                otid,
                PickerKind::TodoRmEntry,
                view.title.clone(),
                ids,
                Some(project),
            );
        }
        None => {
            dd_finalize_select_card(
                config,
                otid,
                crate::i18n::tr(lang, "select.todoRmAllDoneCard"),
            )
            .await;
            remove_picker(state, "dingding", otid);
        }
    }
}

/// TG/Slack 逐条删除卡点「删除」：出队 + 就地编辑刷新（空 → 定格）。
pub(super) async fn select_pick_todo_rm_entry_inplace(
    state: &Arc<ServerState>,
    channel_id: &str,
    mid: &str,
    entry_id: &str,
    picker: &PickerEntry,
    config: &AppConfig,
    lang: Lang,
) {
    let (project, view) = rm_entry_delete(picker, entry_id, lang);
    match view {
        Some((view, ids)) => {
            refresh_select_card_edit(channel_id, config, mid, &view, lang).await;
            morph_picker(
                state,
                channel_id,
                mid,
                PickerKind::TodoRmEntry,
                view.title.clone(),
                ids,
                Some(project),
            );
        }
        None => {
            finalize_select_card_edit(
                channel_id,
                config,
                mid,
                &crate::select::title_todo_rm_entries(
                    &crate::project::display_name(&project),
                    lang,
                ),
                crate::i18n::tr(lang, "select.todoRmAllDoneCard"),
            )
            .await;
            remove_picker(state, channel_id, mid);
        }
    }
}

// ===== 管理卡「新增」提交 =====

/// 飞书普通待办 / 自动待办卡表单提交：按卡片种类新增一条 + 卡片就地刷新。
/// 无论如何消费 `ack`（无台账匹配 → 空 ACK 静默，D7）。
pub(super) async fn fs_todo_manage_submit(
    state: &Arc<ServerState>,
    data: &serde_json::Value,
    ack: crate::feishu::router::CardAck,
) {
    let Some(submit) = crate::feishu::card::parse_card_submit(data, &[]) else {
        let _ = ack.send(None);
        return;
    };
    let picker = {
        let pickers = state.select.pickers.lock().unwrap();
        pickers
            .iter()
            .find(|p| {
                p.channel == "feishu"
                    && p.message_id == submit.message_id
                    && matches!(p.kind, PickerKind::TodoManage | PickerKind::TodoAutoEntry)
            })
            .cloned()
    };
    let Some(picker) = picker else {
        let _ = ack.send(None);
        return;
    };
    let auto = picker.kind == PickerKind::TodoAutoEntry;
    let project = if auto {
        picker.payload.clone()
    } else {
        manage_payload(&picker).map(|payload| payload.project)
    };
    let Some(project) = project else {
        let _ = ack.send(None);
        return;
    };
    if let Some(text) = &submit.user_input {
        if auto {
            let _ = crate::todos::add_auto(&project, text);
        } else {
            let _ = crate::todos::add(&project, text);
        }
    }
    // 空输入提交：不新增，仅刷新列表（顺带同步其它进程的增删结果）。
    let lang = Lang::current();
    if auto {
        let (view, ids) = todo_auto_view_or_empty(&project, lang);
        let card = fs_todo_auto_card(&view, lang);
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
        morph_picker(
            state,
            "feishu",
            &submit.message_id,
            PickerKind::TodoAutoEntry,
            view.title.clone(),
            ids,
            Some(project),
        );
    } else {
        let card = fs_manage_card(&project, lang);
        let _ = ack.send(Some(crate::feishu::card::callback_update_card(card)));
    }
}

/// 钉钉待办管理卡提交（提问卡模板）：同步 ACK 已由路由任务按「提交成功」回包（置灰点击者），
/// 此处新增 + 经 OpenAPI 刷新列表并复位表单（`submitted=false`）以便连续新增。返回是否已处理。
pub(super) async fn handle_todo_dd_submit(
    state: &Arc<ServerState>,
    data: &serde_json::Value,
) -> bool {
    let Some(submit) = crate::dingtalk::card::parse_card_submit(data) else {
        return false;
    };
    let picker = {
        let pickers = state.select.pickers.lock().unwrap();
        pickers
            .iter()
            .find(|p| {
                p.channel == "dingding"
                    && p.message_id == submit.out_track_id
                    && p.kind == PickerKind::TodoManage
            })
            .cloned()
    };
    let Some(picker) = picker else {
        return false;
    };
    let Some(payload) = manage_payload(&picker) else {
        return true;
    };
    if let Some(text) = &submit.user_input {
        if payload.auto {
            let _ = crate::todos::add_auto(&payload.project, text);
        } else {
            let _ = crate::todos::add(&payload.project, text);
        }
    }
    let lang = Lang::current();
    let config = state.config_snapshot();
    if let Ok(client) = crate::dingtalk::client::DingTalkClient::new(&config.channels.dingding) {
        if let Err(err) = client
            .update_card_private(
                &picker.message_id,
                serde_json::json!({
                    "markdown": if payload.auto {
                        crate::i18n::tr(lang, "todoIm.autoAddCardHint").to_string()
                    } else {
                        dd_manage_markdown(&payload.project, lang)
                    },
                    "submit_status": "",
                }),
                serde_json::json!({ "submitted": "false", "private_input": "" }),
            )
            .await
        {
            log(&format!(
                "todo: refresh dingtalk manage card failed: {}",
                err
            ));
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(
        path: &str,
        pinned: bool,
        last_used_at: u64,
    ) -> crate::agents::workspaces::Workspace {
        crate::agents::workspaces::Workspace {
            path: path.to_string(),
            label: crate::project::display_name(path),
            last_used_at,
            agents: Vec::new(),
            pinned,
            hidden: false,
        }
    }

    #[test]
    fn todo_projects_prefer_working_then_idle_then_pinned_then_recent() {
        let snapshot = serde_json::json!([
            {"state": "idle", "cwd": "/projects/idle"},
            {"state": "working", "cwd": "/projects/working"},
            {"state": "ended", "cwd": "/projects/ended"}
        ]);
        let workspaces = vec![
            workspace("/projects/recent", false, 20),
            workspace("/projects/pinned", true, 1),
            workspace("/projects/working", false, 2),
            workspace("/projects/idle", false, 3),
        ];
        let todos = std::collections::HashMap::from([(
            "/projects/todo-only".to_string(),
            vec![crate::todos::TodoEntry {
                id: "t1".into(),
                text: "one".into(),
                created_at_ms: 1,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            }],
        )]);

        let candidates = todo_project_candidates(&snapshot, workspaces, &todos);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.key.as_str())
                .collect::<Vec<_>>(),
            vec![
                "/projects/working",
                "/projects/idle",
                "/projects/pinned",
                "/projects/recent",
                "/projects/todo-only",
            ]
        );
        assert_eq!(candidates[0].activity_rank, 0);
        assert_eq!(candidates[1].activity_rank, 1);
        assert_eq!(candidates[4].todo_count, 1);
    }

    #[test]
    fn compact_todo_projects_show_five_then_more() {
        let candidates = (0..7)
            .map(|index| TodoProjectCandidate {
                key: format!("/projects/p{index}"),
                pinned: index == 0,
                last_used_at: 7 - index,
                has_workspace: true,
                activity_rank: 2,
                todo_count: usize::from(index == 1),
            })
            .collect();
        let options = todo_project_options(candidates, true, Lang::Zh);
        assert_eq!(options.len(), 6);
        assert_eq!(options[0].primary, "★ p0");
        assert_eq!(options[1].badge.as_deref(), Some("· 1 条待办"));
        assert_eq!(options[5].id, crate::select::MORE_OPTION_ID);
    }
}
