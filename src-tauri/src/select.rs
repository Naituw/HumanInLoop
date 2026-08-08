//! 通用「单选卡」（spec `docs/specs/im-select-card.md`）：与传输无关的纯逻辑。
//!
//! 一张单选卡 = `标题 + 一组选项 + 一个动作`。每个选项 = `{稳定 id, 状态圆点, 展示编号, 主文本,
//! 徽标, 次行}`；命令侧只负责「给出选项列表 + 选中后做什么」，各渠道渲染器（飞书
//! `feishu/card.rs::build_select_card`，后续 TG/Slack/钉钉）把它渲染成一行行「信息 + 触发按钮」，
//! 单击即触发（回调 value `{select:<idx>}`）。
//!
//! 渲染布局（飞书，用户定稿「方案A」）：每个选项一行 = 左侧小字号两行富文本
//! （第一行 `圆点 [编号] 主文本 · 徽标`，第二行灰色次行＝标题）+ 右侧一枚紧凑按钮（文案随动作）。
//! 数据保持「无标记语言」：圆点/加粗/颜色等标记由各渠道渲染器自行拼装（跨渠道可各异）。

use crate::i18n::{self, Lang};
use serde_json::Value;
use std::collections::HashSet;

/// 单卡最多渲染的选项数（超出截断并在标题追加说明）。日常 agent 数一般 < 10。
pub const SELECT_MAX_OPTIONS: usize = 20;
pub const MORE_OPTION_ID: &str = "__askhuman_show_more__";

pub fn option_button_label(option: &SelectOption, action: SelectAction, lang: Lang) -> String {
    if option.id == MORE_OPTION_ID {
        match lang {
            Lang::En => "Show more",
            Lang::Zh => "显示更多",
        }
        .to_string()
    } else {
        action.button_label(lang)
    }
}

/// 选项状态圆点（agent 场景）。渲染器映射到各渠道的颜色/字符（飞书 = markdown 彩色 `●`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectDot {
    /// 工作中（绿）。
    Working,
    /// 空闲（灰）。
    Idle,
}

/// 单选卡的动作种类（整卡统一）：决定每行触发按钮的文案与样式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectAction {
    TaskWorkspace,
    TaskAgent,
    TaskPermission,
    TaskInputSource,
    Fork,
    Watch,
    Status,
    Unwatch,
    /// 发送插话（`/msg` 无编号时的选择卡；按钮「发送」，点它把预存内容发给该 agent）。
    Msg,
    /// 空 `/msg` 的目标选择；点选后进入一次性输入卡，不在本次点击发送正文。
    MsgTarget,
    /// 导出未暂存 diff。
    Diff,
    /// 发起 stage 确认。
    Stage,
    /// 导出会话 transcript。
    Transcript,
    /// `/todo` 的选项目卡（点它打开项目待办管理卡，或把预存文本加入项目）。
    Todo,
    /// `/todo-rm` 的选项目卡（点它进入该项目的逐条删除选择卡）。
    TodoRm,
    /// 待办逐条删除卡（选项＝待办条目，按钮「删除」红色）。
    TodoRmEntry,
    /// `/todo-auto` 的选项目卡（点它进入切换卡，或把预存文本加入项目并标为自动执行）。
    TodoAuto,
    /// 待办自动执行切换卡（选项＝待办条目，已自动的带 ⚡ 徽标；按钮「切换」，点击即开/关）。
    TodoAutoEntry,
    /// `/yolo` 的会话选择卡（选项＝开着 YOLO 的 Codex 会话；按钮「关闭」，点击即关）。
    Yolo,
}

impl SelectAction {
    /// 按钮本地化文案。
    pub fn button_label(self, lang: Lang) -> String {
        let key = match self {
            SelectAction::TaskWorkspace => "select.btnChoose",
            SelectAction::TaskAgent => "select.btnChoose",
            SelectAction::TaskPermission => "select.btnChoose",
            SelectAction::TaskInputSource => "select.btnChoose",
            SelectAction::Fork => "select.btnFork",
            SelectAction::Watch => "select.btnWatch",
            SelectAction::Status => "select.btnStatus",
            SelectAction::Unwatch => "select.btnUnwatch",
            SelectAction::Msg => "select.btnMsg",
            SelectAction::MsgTarget => "select.btnChoose",
            SelectAction::Diff => "select.btnDiff",
            SelectAction::Stage => "select.btnStage",
            SelectAction::Transcript => "select.btnTranscript",
            SelectAction::Todo => "select.btnTodo",
            SelectAction::TodoRm => "select.btnChoose",
            SelectAction::TodoRmEntry => "select.btnTodoRmEntry",
            SelectAction::TodoAuto => "select.btnChoose",
            SelectAction::TodoAutoEntry => "select.btnTodoAutoEntry",
            SelectAction::Yolo => "select.btnYoloOff",
        };
        i18n::tr(lang, key).to_string()
    }
}

/// 一个可点选项（传输无关，无标记语言）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectOption {
    /// 稳定标识（agent 场景 = session_id）。点击后据此定位领域对象（不用会漂移的展示编号）。
    pub id: String,
    /// 状态圆点（None = 不显示圆点）。
    pub dot: Option<SelectDot>,
    /// 展示编号（`[n]`，None = 不显示）。
    pub seq: Option<u64>,
    /// 主文本（编号之后、徽标之前）：agent 场景 = `类型 · 工作目录名`。
    pub primary: String,
    /// 主行末徽标（如「· 关注中」，None = 无）。
    pub badge: Option<String>,
    /// Cumulative active time rendered after the badge. `None` means the snapshot did not carry
    /// `activeElapsedSecs`; values under one minute still render in seconds.
    pub elapsed: Option<String>,
    /// 次行（灰、可换行）：agent 场景 = 标题。
    pub secondary: Option<String>,
}

/// 一张单选卡的渲染数据（渲染器消费）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectView {
    /// 卡片标题 / 提示。
    pub title: String,
    /// 选项列表（已按上限截断）。
    pub options: Vec<SelectOption>,
    /// 截断说明（选项超上限时的「（仅列前 N 个）」，否则 None）。
    pub truncated_note: Option<String>,
    /// 动作（决定每行按钮文案/样式）。
    pub action: SelectAction,
}

/// 组装单选卡视图：超 `SELECT_MAX_OPTIONS` 时截断并置截断说明。
pub fn build_view(
    title: String,
    mut options: Vec<SelectOption>,
    action: SelectAction,
    lang: Lang,
) -> SelectView {
    let truncated_note = if options.len() > SELECT_MAX_OPTIONS {
        options.truncate(SELECT_MAX_OPTIONS);
        Some(i18n::tr(lang, "select.truncated").replace("{n}", &SELECT_MAX_OPTIONS.to_string()))
    } else {
        None
    };
    SelectView {
        title,
        options,
        truncated_note,
        action,
    }
}

// ── 命令种类标题（本地化）──

pub fn title_watch(lang: Lang) -> String {
    i18n::tr(lang, "select.titleWatch").to_string()
}
pub fn title_status(lang: Lang) -> String {
    i18n::tr(lang, "select.titleStatus").to_string()
}
pub fn title_fork(lang: Lang) -> String {
    i18n::tr(lang, "select.titleFork").to_string()
}
pub fn title_unwatch(lang: Lang) -> String {
    i18n::tr(lang, "select.titleUnwatch").to_string()
}
pub fn title_msg(lang: Lang) -> String {
    i18n::tr(lang, "select.titleMsg").to_string()
}
pub fn title_diff(lang: Lang) -> String {
    i18n::tr(lang, "select.titleDiff").to_string()
}
pub fn title_stage(lang: Lang) -> String {
    i18n::tr(lang, "select.titleStage").to_string()
}
pub fn title_transcript(lang: Lang) -> String {
    i18n::tr(lang, "select.titleTranscript").to_string()
}
pub fn title_todo(lang: Lang) -> String {
    i18n::tr(lang, "select.titleTodo").to_string()
}
pub fn title_todo_rm(lang: Lang) -> String {
    i18n::tr(lang, "select.titleTodoRm").to_string()
}
pub fn title_yolo(lang: Lang) -> String {
    i18n::tr(lang, "select.titleYolo").to_string()
}

/// `/todo-rm` 逐条删除卡标题：`「<项目名>」的待办（点删除即移除）：`。
pub fn title_todo_rm_entries(project_name: &str, lang: Lang) -> String {
    i18n::tr(lang, "select.titleTodoRmEntries").replace("{project}", project_name)
}

pub fn title_todo_auto(lang: Lang) -> String {
    i18n::tr(lang, "select.titleTodoAuto").to_string()
}

/// `/todo-auto` 切换卡标题：`「<项目名>」的待办（点切换开/关自动执行）：`。
pub fn title_todo_auto_entries(project_name: &str, lang: Lang) -> String {
    i18n::tr(lang, "select.titleTodoAutoEntries").replace("{project}", project_name)
}

/// 由项目待办队列组装逐条删除卡选项：编号＝FIFO 序（1 起），主文本＝待办原文（渲染层自行截断）。
pub fn todo_rm_options(entries: &[crate::todos::TodoEntry]) -> Vec<SelectOption> {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| SelectOption {
            id: e.id.clone(),
            dot: None,
            seq: Some((i + 1) as u64),
            primary: e.text.clone(),
            badge: None,
            elapsed: None,
            secondary: None,
        })
        .collect()
}

/// `/todo-auto` 切换卡选项：同删除卡，但已自动的条目带 ⚡ 徽标（第 17 轮定案）。
pub fn todo_auto_options(entries: &[crate::todos::TodoEntry], lang: Lang) -> Vec<SelectOption> {
    let mark = i18n::tr(lang, "todo.autoMark");
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| SelectOption {
            id: e.id.clone(),
            dot: None,
            seq: Some((i + 1) as u64),
            primary: e.text.clone(),
            badge: e.auto.then(|| mark.to_string()),
            elapsed: None,
            secondary: None,
        })
        .collect()
}

pub fn title_task_workspace(lang: Lang) -> String {
    match lang {
        Lang::En => "Choose a workspace",
        Lang::Zh => "选择工作目录",
    }
    .to_string()
}

pub fn title_task_agent(lang: Lang) -> String {
    match lang {
        Lang::En => "Choose an Agent",
        Lang::Zh => "选择 Agent",
    }
    .to_string()
}

pub fn title_task_permission(lang: Lang) -> String {
    match lang {
        Lang::En => "Choose a permission mode",
        Lang::Zh => "选择权限模式",
    }
    .to_string()
}

pub fn title_task_input_source(lang: Lang) -> String {
    match lang {
        Lang::En => "Choose a task source",
        Lang::Zh => "选择任务来源",
    }
    .to_string()
}

/// Build one agent option from a registry snapshot. Cumulative active time comes precomputed in
/// `activeElapsedSecs`; `now` remains in the shared call shape for compatibility with callers.
fn option_from_record(
    rec: &Value,
    sid: String,
    watching: &HashSet<String>,
    now: u64,
    lang: Lang,
    fork_parent: Option<String>,
) -> SelectOption {
    let dot = match rec.get("state").and_then(|v| v.as_str()) {
        Some("working") => Some(SelectDot::Working),
        Some("idle") => Some(SelectDot::Idle),
        _ => None,
    };
    let seq = rec.get("seq").and_then(|v| v.as_u64());
    let badge = if watching.contains(&sid) {
        Some(i18n::tr(lang, "select.watchingBadge").trim().to_string())
    } else {
        None
    };
    // Picker badges remain limited to Working agents; final Watch cards carry frozen totals.
    let elapsed = (dot == Some(SelectDot::Working))
        .then(|| elapsed_badge(rec, now, lang))
        .flatten();
    SelectOption {
        id: sid,
        dot,
        seq,
        primary: primary_text(rec, lang),
        badge,
        elapsed,
        secondary: Some(title_text(rec, fork_parent.as_deref(), lang)),
    }
}

/// Main-line cumulative active-time badge. Only Working agents call this helper.
fn elapsed_badge(rec: &Value, _now: u64, lang: Lang) -> Option<String> {
    let secs = rec.get("activeElapsedSecs").and_then(|v| v.as_u64())?;
    Some(format!(
        "· {}",
        i18n::tr(lang, "watch.statsActiveElapsed")
            .replace("{t}", &crate::watch::fmt_duration(secs, lang))
    ))
}

/// 主文本 = `类型 · 工作目录名`（工作目录名取 cwd 末段，缺省仅类型）。
fn primary_text(rec: &Value, lang: Lang) -> String {
    let kind = rec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let kind_label = crate::agents::AgentKind::parse(kind)
        .map(|k| k.label().to_string())
        .unwrap_or_else(|| kind.to_string());
    match rec
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(crate::autochannel::project_name)
    {
        Some(dir) => format!("{} · {}", kind_label, dir),
        None => {
            if kind_label.is_empty() {
                i18n::tr(lang, "autoChannel.noProject").to_string()
            } else {
                kind_label
            }
        }
    }
}

/// 次行标题（缺省 → noTitle）。
fn title_text(rec: &Value, fork_parent: Option<&str>, lang: Lang) -> String {
    let title = rec
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| i18n::tr(lang, "autoChannel.noTitle").to_string());
    match fork_parent {
        Some(parent) => format!(
            "{} · {title}",
            i18n::tr(lang, "watch.forkedFrom").replace("{id}", parent)
        ),
        None => title,
    }
}

fn fork_parent_label(snapshot: &[Value], rec: &Value) -> Option<String> {
    let parent_id = rec.get("forkedFromSessionId")?.as_str()?;
    let parent_seq = snapshot.iter().find_map(|candidate| {
        (candidate.get("sessionId").and_then(Value::as_str) == Some(parent_id))
            .then(|| candidate.get("seq").and_then(Value::as_u64))
            .flatten()
    });
    Some(
        parent_seq
            .map(|seq| format!("#{seq}"))
            .unwrap_or_else(|| parent_id.chars().take(8).collect()),
    )
}

/// 由注册表快照（`AgentRegistry::snapshot()` 的 Value 数组）组装 agent 选项：仅取「工作中 / 空闲」
/// （已结束不列），工作中在前。`watching` = 本渠道已在关注的 session_id 集合（命中加「· 关注中」徽标）。
pub fn agent_options(
    snapshot: &Value,
    watching: &HashSet<String>,
    now: u64,
    lang: Lang,
) -> Vec<SelectOption> {
    let empty = Vec::new();
    let list = snapshot.as_array().unwrap_or(&empty);
    let mut working: Vec<SelectOption> = Vec::new();
    let mut idle: Vec<SelectOption> = Vec::new();
    for rec in list {
        let bucket = match rec.get("state").and_then(|v| v.as_str()) {
            Some("working") => &mut working,
            Some("idle") => &mut idle,
            _ => continue, // ended / 未知：不列
        };
        let sid = rec
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if sid.is_empty() {
            continue;
        }
        bucket.push(option_from_record(
            rec,
            sid,
            watching,
            now,
            lang,
            fork_parent_label(list, rec),
        ));
    }
    working.extend(idle);
    working
}

/// Native-fork candidates are active records whose Agent family passed the runtime capability
/// probe and whose canonical working directory is still available.
pub fn fork_options(
    snapshot: &Value,
    ready_kinds: &HashSet<crate::agents::AgentKind>,
    now: u64,
    lang: Lang,
) -> Vec<SelectOption> {
    let empty = Vec::new();
    let list = snapshot.as_array().unwrap_or(&empty);
    let mut working = Vec::new();
    let mut idle = Vec::new();
    for rec in list {
        let bucket = match rec.get("state").and_then(Value::as_str) {
            Some("working") => &mut working,
            Some("idle") => &mut idle,
            _ => continue,
        };
        let Some(kind) = rec
            .get("kind")
            .and_then(Value::as_str)
            .and_then(crate::agents::AgentKind::parse)
        else {
            continue;
        };
        if !ready_kinds.contains(&kind) {
            continue;
        }
        let Some(cwd) = rec.get("cwd").and_then(Value::as_str) else {
            continue;
        };
        if !std::path::Path::new(cwd).is_dir() {
            continue;
        }
        let sid = rec
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if sid.is_empty() || crate::agents::transcript_full::transcript_mtime(kind, &sid).is_none()
        {
            continue;
        }
        bucket.push(option_from_record(
            rec,
            sid,
            &HashSet::new(),
            now,
            lang,
            fork_parent_label(list, rec),
        ));
    }
    working.extend(idle);
    working
}

/// 由注册表快照组装 `/watch` 单选卡选项：**仅列「工作中」** 的 agent（含 grok，区别于
/// `msg_options` 排除 grok）。空闲 agent 关注没有实际意义，故不列出。
pub fn watch_options(
    snapshot: &Value,
    watching: &HashSet<String>,
    now: u64,
    lang: Lang,
) -> Vec<SelectOption> {
    let empty = Vec::new();
    let list = snapshot.as_array().unwrap_or(&empty);
    let mut out: Vec<SelectOption> = Vec::new();
    for rec in list {
        if rec.get("state").and_then(|v| v.as_str()) != Some("working") {
            continue;
        }
        let sid = rec
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if sid.is_empty() {
            continue;
        }
        out.push(option_from_record(
            rec,
            sid,
            watching,
            now,
            lang,
            fork_parent_label(list, rec),
        ));
    }
    out
}

/// 由注册表快照组装「可发送插话」的候选选项（`/msg` 无编号单选卡）：**仅列「工作中」且非 grok**
/// 的 agent（插话只对工作中有意义、grok 无可靠传话通道）。`watching` 命中仍加「· 关注中」徽标。
pub fn msg_options(
    snapshot: &Value,
    watching: &HashSet<String>,
    now: u64,
    lang: Lang,
) -> Vec<SelectOption> {
    let empty = Vec::new();
    let list = snapshot.as_array().unwrap_or(&empty);
    let mut out: Vec<SelectOption> = Vec::new();
    for rec in list {
        if rec.get("state").and_then(|v| v.as_str()) != Some("working") {
            continue; // 仅工作中。
        }
        if rec.get("kind").and_then(|v| v.as_str()) == Some("grok") {
            continue; // grok 无法插话。
        }
        let sid = rec
            .get("sessionId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if sid.is_empty() {
            continue;
        }
        out.push(option_from_record(
            rec,
            sid,
            watching,
            now,
            lang,
            fork_parent_label(list, rec),
        ));
    }
    out
}

/// 组装单个 agent 选项（`/unwatch` 单选卡按订阅列举时用）：按 session_id 在快照定位记录；
/// 记录已消失（agent 结束/被清理）时以 `seq` 兜底、圆点/主文本降级。
pub fn agent_option_by_session(
    snapshot: &Value,
    session_id: &str,
    seq: u64,
    now: u64,
    lang: Lang,
) -> SelectOption {
    if let Some(rec) = snapshot.as_array().and_then(|l| {
        l.iter()
            .find(|r| r.get("sessionId").and_then(|v| v.as_str()) == Some(session_id))
    }) {
        let mut opt = option_from_record(
            rec,
            session_id.to_string(),
            &HashSet::new(),
            now,
            lang,
            snapshot
                .as_array()
                .and_then(|list| fork_parent_label(list, rec)),
        );
        // 订阅侧的稳定展示编号优先（快照 seq 与订阅 seq 一致，缺省时兜底）。
        opt.seq = opt.seq.or(Some(seq));
        opt
    } else {
        SelectOption {
            id: session_id.to_string(),
            dot: None,
            seq: Some(seq),
            primary: i18n::tr(lang, "autoChannel.noProject").to_string(),
            badge: None,
            elapsed: None,
            secondary: Some(i18n::tr(lang, "autoChannel.noTitle").to_string()),
        }
    }
}

/// 组装单个 YOLO 会话选项（`/yolo` 单选卡，spec codex-permission-remember D53）：按
/// session_id 在快照定位记录，与其余 agent 卡同口径（主文本 = 类型 · 项目名，副文本 =
/// 标题，圆点 / 编号 / 工作时长），外加 YOLO 徽标。不在册（agent 已结束，规则未过期仍可
/// 关）时用注册表存档的 `(标题, 项目名)` 降级；YOLO 仅 Codex 会话，类型标签固定。
pub fn yolo_option_by_session(
    snapshot: &Value,
    session_id: &str,
    display: Option<(String, String)>,
    now: u64,
    lang: Lang,
) -> SelectOption {
    let badge = Some("YOLO".to_string());
    if let Some(rec) = snapshot.as_array().and_then(|l| {
        l.iter()
            .find(|r| r.get("sessionId").and_then(|v| v.as_str()) == Some(session_id))
    }) {
        let mut opt = option_from_record(
            rec,
            session_id.to_string(),
            &HashSet::new(),
            now,
            lang,
            snapshot
                .as_array()
                .and_then(|list| fork_parent_label(list, rec)),
        );
        opt.badge = badge;
        return opt;
    }
    let kind_label = crate::agents::AgentKind::Codex.label();
    let (title, project) = display.unwrap_or_default();
    SelectOption {
        id: session_id.to_string(),
        dot: None,
        seq: None,
        primary: if project.is_empty() {
            kind_label.to_string()
        } else {
            format!("{kind_label} · {project}")
        },
        badge,
        elapsed: None,
        secondary: Some(if title.is_empty() {
            i18n::tr(lang, "autoChannel.noTitle").to_string()
        } else {
            title
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Fixed call-time value retained by the public option-builder API.
    const NOW: u64 = 1_000_000;

    fn snap() -> Value {
        json!([
            {"seq":1,"kind":"cursor","sessionId":"s-idle","state":"idle","title":"闲着","cwd":"/tmp/my-proj","activeElapsedSecs":600},
            {"seq":2,"kind":"claude","sessionId":"s-work","state":"working","title":"忙着","cwd":"/tmp/api-server","activeElapsedSecs":360},
            {"seq":3,"kind":"codex","sessionId":"s-end","state":"ended","title":"完了","cwd":"/tmp/proj","activeElapsedSecs":100},
        ])
    }

    #[test]
    fn yolo_option_matches_agent_card_shape_with_fallback() {
        // 在册：与其余 agent 卡同口径（类型 · 项目 / 标题 / 圆点 / 编号）+ YOLO 徽标。
        let opt = yolo_option_by_session(&snap(), "s-work", None, NOW, Lang::Zh);
        assert_eq!(opt.primary, "Claude Code · api-server");
        assert_eq!(opt.secondary.as_deref(), Some("忙着"));
        assert_eq!(opt.dot, Some(SelectDot::Working));
        assert_eq!(opt.seq, Some(2));
        assert_eq!(opt.badge.as_deref(), Some("YOLO"));
        // 不在册：注册表存档的 (标题, 项目名) 降级，类型标签固定 Codex。
        let opt = yolo_option_by_session(
            &snap(),
            "s-gone",
            Some(("修权限弹窗".to_string(), "my-proj".to_string())),
            NOW,
            Lang::Zh,
        );
        assert_eq!(opt.primary, "Codex · my-proj");
        assert_eq!(opt.secondary.as_deref(), Some("修权限弹窗"));
        assert_eq!(opt.dot, None);
        assert_eq!(opt.seq, None);
        assert_eq!(opt.badge.as_deref(), Some("YOLO"));
        // 完全无存档信息：仅类型 + 无标题占位。
        let opt = yolo_option_by_session(&snap(), "s-gone", None, NOW, Lang::Zh);
        assert_eq!(opt.primary, "Codex");
        assert_eq!(
            opt.secondary.as_deref(),
            Some(i18n::tr(Lang::Zh, "autoChannel.noTitle"))
        );
    }

    #[test]
    fn agent_options_working_first_and_skips_ended() {
        let opts = agent_options(&snap(), &HashSet::new(), NOW, Lang::Zh);
        // 只列工作中 + 空闲；工作中在前。
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id, "s-work");
        assert_eq!(opts[0].dot, Some(SelectDot::Working));
        assert_eq!(opts[0].seq, Some(2));
        // 主文本 = 类型 · 工作目录名。
        assert_eq!(opts[0].primary, "Claude Code · api-server");
        assert_eq!(opts[0].secondary.as_deref(), Some("忙着"));
        assert_eq!(opts[0].elapsed.as_deref(), Some("· 累计工作 6 分钟"));
        assert_eq!(opts[1].id, "s-idle");
        assert_eq!(opts[1].dot, Some(SelectDot::Idle));
        assert_eq!(opts[1].primary, "Cursor · my-proj");
        // 空闲态不显示运行时长（用户定案：易误导）。
        assert_eq!(opts[1].elapsed, None);
        // 无徽标。
        assert!(opts[0].badge.is_none());
    }

    #[test]
    fn agent_options_mark_forked_sessions_with_parent_sequence() {
        let mut snapshot = snap();
        snapshot.as_array_mut().unwrap().push(json!({
            "seq": 4,
            "kind": "codex",
            "sessionId": "s-child",
            "forkedFromSessionId": "s-work",
            "state": "working",
            "title": "忙着",
            "cwd": "/tmp/api-server",
            "activeElapsedSecs": 30
        }));
        let opts = agent_options(&snapshot, &HashSet::new(), NOW, Lang::Zh);
        let child = opts.iter().find(|option| option.id == "s-child").unwrap();
        assert_eq!(child.secondary.as_deref(), Some("从 #2 分叉 · 忙着"));
        let parent = opts.iter().find(|option| option.id == "s-work").unwrap();
        assert_eq!(parent.secondary.as_deref(), Some("忙着"));
    }

    #[test]
    fn elapsed_shows_seconds_under_a_minute_and_none_without_active_time() {
        let snap = json!([
            {"seq":1,"kind":"claude","sessionId":"s1","state":"working","title":"t","cwd":"/tmp/a","activeElapsedSecs":30},
            {"seq":2,"kind":"cursor","sessionId":"s2","state":"working","title":"t","cwd":"/tmp/b"},
        ]);
        let opts = agent_options(&snap, &HashSet::new(), NOW, Lang::Zh);
        assert_eq!(opts[0].elapsed.as_deref(), Some("· 累计工作 30 秒"));
        assert_eq!(opts[1].elapsed, None);
    }

    #[test]
    fn watching_badge_applied() {
        let mut watching = HashSet::new();
        watching.insert("s-work".to_string());
        let opts = agent_options(&snap(), &watching, NOW, Lang::Zh);
        assert_eq!(opts[0].id, "s-work");
        assert_eq!(opts[0].badge.as_deref(), Some("· 关注中"));
        assert!(opts[1].badge.is_none());
    }

    #[test]
    fn agent_option_by_session_falls_back_when_missing() {
        let opt = agent_option_by_session(&snap(), "s-gone", 7, NOW, Lang::Zh);
        assert_eq!(opt.id, "s-gone");
        assert_eq!(opt.seq, Some(7));
        assert!(opt.dot.is_none());
        assert!(opt.elapsed.is_none());
        // 命中时用快照字段。
        let opt2 = agent_option_by_session(&snap(), "s-work", 2, NOW, Lang::Zh);
        assert_eq!(opt2.dot, Some(SelectDot::Working));
        assert_eq!(opt2.primary, "Claude Code · api-server");
        assert_eq!(opt2.elapsed.as_deref(), Some("· 累计工作 6 分钟"));
    }

    #[test]
    fn build_view_truncates_over_limit() {
        let mk = |i: usize| SelectOption {
            id: i.to_string(),
            dot: None,
            seq: Some(i as u64),
            primary: format!("opt {i}"),
            badge: None,
            elapsed: None,
            secondary: None,
        };
        let many: Vec<SelectOption> = (0..(SELECT_MAX_OPTIONS + 5)).map(mk).collect();
        let v = build_view("T".into(), many, SelectAction::Watch, Lang::Zh);
        assert_eq!(v.options.len(), SELECT_MAX_OPTIONS);
        assert!(v.truncated_note.is_some());
        // 未超上限不截断。
        let v2 = build_view("T".into(), vec![mk(0)], SelectAction::Status, Lang::Zh);
        assert_eq!(v2.options.len(), 1);
        assert!(v2.truncated_note.is_none());
    }

    #[test]
    fn todo_rm_options_are_numbered_by_fifo_order() {
        let entries = vec![
            crate::todos::TodoEntry {
                id: "id-a".into(),
                text: "修复登录".into(),
                created_at_ms: 1,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            },
            crate::todos::TodoEntry {
                id: "id-b".into(),
                text: "写文档".into(),
                created_at_ms: 2,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            },
        ];
        let opts = todo_rm_options(&entries);
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id, "id-a");
        assert_eq!(opts[0].seq, Some(1));
        assert_eq!(opts[0].primary, "修复登录");
        assert!(opts[0].dot.is_none() && opts[0].secondary.is_none());
        assert_eq!(opts[1].seq, Some(2));
        // 标题带项目名。
        assert!(title_todo_rm_entries("proj", Lang::Zh).contains("proj"));
    }

    #[test]
    fn todo_auto_options_badge_marks_auto_entries_only() {
        let entries = vec![
            crate::todos::TodoEntry {
                id: "id-a".into(),
                text: "修复登录".into(),
                created_at_ms: 1,
                agent_kind: None,
                auto: true,
                attachments: Vec::new(),
            },
            crate::todos::TodoEntry {
                id: "id-b".into(),
                text: "写文档".into(),
                created_at_ms: 2,
                agent_kind: None,
                auto: false,
                attachments: Vec::new(),
            },
        ];
        let opts = todo_auto_options(&entries, Lang::Zh);
        assert_eq!(opts.len(), 2);
        // 已自动的带 ⚡ 徽标；未自动的无徽标。
        assert!(opts[0].badge.as_deref().unwrap_or_default().contains('⚡'));
        assert!(opts[1].badge.is_none());
        assert_eq!(opts[0].seq, Some(1));
        assert!(title_todo_auto_entries("proj", Lang::Zh).contains("proj"));
    }

    #[test]
    fn action_button_labels() {
        assert_eq!(SelectAction::Watch.button_label(Lang::Zh), "关注");
        assert_eq!(SelectAction::Status.button_label(Lang::Zh), "查看");
        assert_eq!(SelectAction::Unwatch.button_label(Lang::Zh), "取消");
        assert_eq!(SelectAction::Msg.button_label(Lang::Zh), "发送");
        assert_eq!(SelectAction::MsgTarget.button_label(Lang::Zh), "选择");
        assert_eq!(SelectAction::Diff.button_label(Lang::Zh), "差异");
        assert_eq!(SelectAction::Stage.button_label(Lang::Zh), "暂存");
        assert_eq!(SelectAction::Transcript.button_label(Lang::Zh), "会话");
        assert_eq!(SelectAction::Todo.button_label(Lang::Zh), "待办");
        assert_eq!(SelectAction::TodoRm.button_label(Lang::Zh), "选择");
        assert_eq!(SelectAction::TodoRmEntry.button_label(Lang::Zh), "删除");
        assert_eq!(SelectAction::TodoAuto.button_label(Lang::Zh), "选择");
        assert_eq!(SelectAction::TodoAutoEntry.button_label(Lang::Zh), "切换");
        let more = SelectOption {
            id: MORE_OPTION_ID.into(),
            dot: None,
            seq: None,
            primary: "more".into(),
            badge: None,
            elapsed: None,
            secondary: None,
        };
        assert_eq!(
            option_button_label(&more, SelectAction::TaskWorkspace, Lang::Zh),
            "显示更多"
        );
    }

    #[test]
    fn watch_options_only_working_including_grok() {
        let mut snap = snap();
        snap.as_array_mut().unwrap().push(json!({
            "seq":4,"kind":"grok","sessionId":"s-grok","state":"working","title":"g","cwd":"/tmp/g","activeElapsedSecs":120
        }));
        let opts = watch_options(&snap, &HashSet::new(), NOW, Lang::Zh);
        // working cursor (s-work) 不在 snap() 默认里——snap() 里 claude s-work 是 working。
        // snap() = idle cursor + working claude + ended codex + 追加 working grok。
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id, "s-work"); // claude，working
        assert_eq!(opts[1].id, "s-grok"); // grok，working（不排除）
                                          // idle 的 s-idle 不在列表中。
        assert!(!opts.iter().any(|o| o.id == "s-idle"));
        // ended 的 s-end 不在列表中。
        assert!(!opts.iter().any(|o| o.id == "s-end"));
        // 关注徽标生效。
        let mut watching = HashSet::new();
        watching.insert("s-grok".to_string());
        let opts2 = watch_options(&snap, &watching, NOW, Lang::Zh);
        assert_eq!(opts2[1].badge.as_deref(), Some("· 关注中"));
    }

    #[test]
    fn msg_options_only_working_non_grok() {
        // 快照含：working cursor / idle claude / ended codex（见 snap）+ 追加一个 working grok。
        let mut snap = snap();
        snap.as_array_mut().unwrap().push(json!({
            "seq":4,"kind":"grok","sessionId":"s-grok","state":"working","title":"g","cwd":"/tmp/g"
        }));
        let opts = msg_options(&snap, &HashSet::new(), NOW, Lang::Zh);
        // 仅剩工作中·非 grok 的那一个（claude s-work）。
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id, "s-work");
        assert_eq!(opts[0].elapsed.as_deref(), Some("· 累计工作 6 分钟"));
        // 关注徽标仍生效。
        let mut watching = HashSet::new();
        watching.insert("s-work".to_string());
        let opts2 = msg_options(&snap, &watching, NOW, Lang::Zh);
        assert_eq!(opts2[0].badge.as_deref(), Some("· 关注中"));
    }
}
