//! `/watch` 实时关注（spec `docs/specs/im-watch.md`）：与传输无关的纯逻辑。
//!
//! 一次关注 = 一张 IM「实时状态卡」，daemon 引擎按签名变化就地编辑（飞书 / Telegram / Slack；
//! 钉钉待 PoC）。本模块提供：订阅持久化（`~/.askhuman/state/watch.json`）、由注册表快照记录
//! 构建**结构化**渲染「帧」（`WatchFrame`，不含任何渠道标记语言）、帧签名（变化才编辑，跨渠道
//! 一致）、飞书卡片视图文案组装（`card_view`；Telegram/Slack 的渲染在各自渠道模块）、本地时区
//! 绝对时刻格式化。

use crate::autochannel;
use crate::i18n::{self, Lang};
use crate::paths;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 每渠道关注上限。
pub const MAX_WATCHES: usize = 5;

/// Keep an existing watch alive for this long after its agent becomes Idle. This lets a user
/// interrupt one turn and immediately continue in the same session without re-subscribing.
pub const IDLE_GRACE_SECS: u64 = 5 * 60;

/// 渠道是否支持 /watch（就地编辑 + 按钮回调都可用）。四渠道全支持（钉钉经 PoC 验证后
/// M4 接入，`docs/plans/im-watch-channels.md` §4）。
pub fn channel_supported(channel_id: &str) -> bool {
    matches!(channel_id, "feishu" | "telegram" | "slack" | "dingding")
}

/// 持久化的一条关注（跨 daemon 重启恢复后继续编辑同一张卡）。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PersistedWatch {
    /// 渠道 id（feishu / telegram / slack / dingding）。
    pub channel: String,
    /// 被关注 agent 的 session_id（身份键，跨重启稳定；seq 编号不跨重启）。
    pub session_id: String,
    /// 实时状态卡的消息 id（编辑目标；渠道各异，见 daemon `WatchEntry::message_id`）。
    pub message_id: String,
    #[serde(default)]
    pub created_at: u64,
    /// 终态已定格但保留路由供重新关注（仅 AutoStopped；引擎/上限/空闲退出跳过此类 entry）。
    #[serde(default)]
    pub rewatchable: bool,
}

/// 读取持久化订阅（缺失 / 解析失败 → 空）。
pub fn load() -> Vec<PersistedWatch> {
    let Ok(text) = std::fs::read_to_string(paths::watch_file()) else {
        return Vec::new();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// 原子写入订阅列表（临时文件 + rename）。best-effort，失败静默。
pub fn save(items: &[PersistedWatch]) {
    let Ok(json) = serde_json::to_string_pretty(items) else {
        return;
    };
    let path = paths::watch_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// 卡片状态四态（`Waiting` = 该 agent 有在途 AskHuman 提问，覆盖 工作中/空闲）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchPhase {
    Working,
    Idle,
    Waiting,
    Ended,
}

/// 终态卡片的种类（决定禁用按钮的文案）。
///
/// 注：`AutoStopped` 携带「切换目标渠道展示名」用于动态文案，故本枚举**不再是 `Copy`**（改 `Clone`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalKind {
    /// agent 已结束（自动退订）。
    Ended,
    /// 用户主动取消关注。
    Cancelled,
    /// 重复 /watch 换新卡，旧卡被接替。
    Replaced,
    /// 卡片被会话新消息淹没后「跟底」重发，旧卡定格（订阅仍活，由新卡接续）。
    Moved,
    /// 「按需发送」下活跃槽切走本渠道时自动结束关注（`String` = 切换目标渠道展示名 `{to}`）。
    /// 见 `docs/specs/im-auto-end-watch.md`。
    AutoStopped(String),
    /// agent 转为空闲后自动结束关注。
    Idle,
    /// 用户已从该卡点击「重新关注」（旧卡定格用）。
    Rewatched,
}

impl FinalKind {
    /// 该终态是否支持「重新关注」按钮（可点击而非 disabled）。
    /// `AutoStopped`：活跃渠道切走；`Cancelled`：用户主动取消关注。
    pub fn is_rewatchable(&self) -> bool {
        matches!(self, FinalKind::AutoStopped(_) | FinalKind::Cancelled)
    }
}

/// 卡片渲染模式：活动（可交互按钮）或终态（禁用按钮 + 终态文案）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CardMode {
    Active,
    Final(FinalKind),
}

/// 一帧渲染数据（由注册表快照记录 + 等待标志构建）。**结构化、无渠道标记语言**：
/// 各渠道渲染器（飞书 `card_view` / Telegram / Slack）各自消费。签名不含渲染时刻，
/// 故内容不变不编辑。
#[derive(Debug, Clone, PartialEq)]
pub struct WatchFrame {
    /// 展示编号（当前 daemon 生命周期内稳定；重启后重解析）。
    pub seq: u64,
    /// 家族展示名（Cursor / Claude Code / …）。
    pub kind_label: String,
    /// Short direct-parent session id for lightweight lineage display.
    pub forked_from: Option<String>,
    /// 会话标题（无则 None）。
    pub title: Option<String>,
    /// 项目名（cwd 末段；无则 None）。
    pub project: Option<String>,
    pub phase: WatchPhase,
    /// 最后一段助手文字（transcript 尾部，已截断）。
    pub text: Option<String>,
    /// 足迹时间线（≤3 步，旧→新；最后一段文字之后的调用）。
    pub steps: Vec<crate::agents::activity::ToolStep>,
    /// 文字之后被挤出时间线的更早调用数（>0 时在时间线上方标注「… 已省略 N 步」）。
    pub steps_omitted: usize,
    /// 当前 TODO 清单（agent 未用 todo 功能则空）。
    pub todos: Vec<crate::agents::activity::TodoItem>,
    /// Cumulative active time for the session, excluding Idle intervals. **Not part of the
    /// signature** so the clock never causes edits by itself.
    pub active_elapsed_secs: Option<u64>,
    /// 活动时刻（Unix 秒；进「最近动态」标签）。
    pub at: Option<u64>,
}

/// 由注册表快照的一条记录构建帧。`rec=None`（记录已彻底消失）→ 视为已结束的最小帧。
/// `waiting`：该 agent 是否有在途 AskHuman 提问（覆盖 工作中/空闲）。
pub fn build_frame(seq: u64, rec: Option<&Value>, waiting: bool) -> WatchFrame {
    let Some(rec) = rec else {
        return WatchFrame {
            seq,
            kind_label: String::new(),
            forked_from: None,
            title: None,
            project: None,
            phase: WatchPhase::Ended,
            text: None,
            steps: Vec::new(),
            steps_omitted: 0,
            todos: Vec::new(),
            active_elapsed_secs: None,
            at: None,
        };
    };
    let kind = rec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let kind_label = crate::agents::AgentKind::parse(kind)
        .map(|k| k.label().to_string())
        .unwrap_or_else(|| kind.to_string());
    let title = rec
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    let project = rec
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(project_name);
    let state = rec.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let phase = match state {
        "ended" => WatchPhase::Ended,
        _ if waiting => WatchPhase::Waiting,
        "working" => WatchPhase::Working,
        _ => WatchPhase::Idle,
    };
    let forked_from = rec
        .get("forkedFromSessionId")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(8).collect());
    // 已结束的会话不再读 transcript（内容定格在结束前最后一帧的签名上无意义——终态卡会展示
    // 最后已知活动；这里仍解析一次，让终态卡带上收尾内容）。
    let parts = autochannel::activity_parts(rec);
    WatchFrame {
        seq,
        kind_label,
        forked_from,
        title,
        project,
        phase,
        text: parts.text,
        steps: parts.steps,
        steps_omitted: parts.steps_omitted,
        todos: parts.todos,
        active_elapsed_secs: rec.get("activeElapsedSecs").and_then(|v| v.as_u64()),
        at: parts.at,
    }
}

/// Whether an Idle record has exhausted the grace period for an existing watch subscription.
/// `lastActivity` is the persisted Idle boundary, so daemon restarts do not reset the deadline.
/// Missing timestamps expire defensively rather than keeping a malformed subscription forever.
pub fn idle_grace_expired(rec: Option<&Value>, now: u64) -> bool {
    let Some(rec) = rec else {
        return false;
    };
    if rec.get("state").and_then(Value::as_str) != Some("idle") {
        return false;
    }
    rec.get("lastActivity")
        .and_then(Value::as_u64)
        .is_none_or(|idle_since| now.saturating_sub(idle_since) >= IDLE_GRACE_SECS)
}

/// Automatic terminal state for an existing subscription. Explicit session end remains immediate;
/// Idle is terminal only after its grace period. Waiting deliberately overrides an Idle record.
pub fn automatic_final_kind(
    frame: &WatchFrame,
    rec: Option<&Value>,
    now: u64,
) -> Option<FinalKind> {
    match frame.phase {
        WatchPhase::Ended => Some(FinalKind::Ended),
        WatchPhase::Idle if idle_grace_expired(rec, now) => Some(FinalKind::Idle),
        WatchPhase::Working | WatchPhase::Idle | WatchPhase::Waiting => None,
    }
}

/// Cross-channel signature of user-visible content. Activity timestamps and cumulative active
/// time are intentionally excluded so clocks cannot trigger edits when content is unchanged.
pub fn signature(f: &WatchFrame) -> String {
    use std::fmt::Write;
    let mut s = format!(
        "{:?}|{}|{}|{}|",
        f.phase,
        f.title.as_deref().unwrap_or(""),
        f.text.as_deref().unwrap_or(""),
        f.steps_omitted,
    );
    for st in &f.steps {
        let _ = write!(
            s,
            "{:?};{:?};{}\u{1f}",
            st.state,
            st.tool.label,
            st.tool.object.as_deref().unwrap_or("")
        );
    }
    s.push('|');
    for td in &f.todos {
        let _ = write!(s, "{:?};{}\u{1f}", td.state, td.content);
    }
    let _ = write!(s, "|{}|{}", f.seq, f.forked_from.as_deref().unwrap_or(""));
    s
}

// ── 渠道共享的本地化文案构件（飞书 `card_view` 与 Telegram/Slack 渲染器共用）──

/// 头部行：`实时关注 [3] Cursor — HumanInLoop`。
pub fn header_text(f: &WatchFrame, lang: Lang) -> String {
    let agent_label: &str = if f.kind_label.is_empty() {
        i18n::tr(lang, "autoChannel.noTitle")
    } else {
        f.kind_label.as_str()
    };
    i18n::tr(lang, "watch.cardHeader")
        .replace("{id}", &f.seq.to_string())
        .replace("{agent}", agent_label)
        .replace(
            "{project}",
            f.project
                .as_deref()
                .unwrap_or(i18n::tr(lang, "autoChannel.noProject")),
        )
}

/// Status line with cumulative active time. The total spans turns, excludes true Idle intervals,
/// and continues while Waiting. Values under one minute remain hidden on Watch cards.
pub fn state_line_text(f: &WatchFrame, _now: u64, lang: Lang) -> String {
    let mut state_line = match f.phase {
        WatchPhase::Working => i18n::tr(lang, "watch.stateWorking"),
        WatchPhase::Idle => i18n::tr(lang, "watch.stateIdle"),
        WatchPhase::Waiting => i18n::tr(lang, "watch.stateWaiting"),
        WatchPhase::Ended => i18n::tr(lang, "watch.stateEnded"),
    }
    .to_string();
    if let Some(elapsed) = f.active_elapsed_secs {
        if elapsed >= 60 {
            state_line.push_str(" · ");
            state_line.push_str(
                &i18n::tr(lang, "watch.statsActiveElapsed")
                    .replace("{t}", &fmt_duration(elapsed, lang)),
            );
        }
    }
    if let Some(parent) = f.forked_from.as_deref() {
        state_line.push_str(" · ");
        state_line.push_str(&i18n::tr(lang, "watch.forkedFrom").replace("{id}", parent));
    }
    state_line
}

/// 「最近动态（14:32:05）：」——绝对时刻（卡片只在变化时编辑，相对时间会失真）。
pub fn activity_heading_text(f: &WatchFrame, now: u64, lang: Lang) -> String {
    let heading = i18n::tr(lang, "autoChannel.activityHeading");
    match (lang, f.at) {
        (Lang::Zh, Some(at)) => format!("{heading}（{}）：", fmt_local_time(at, now)),
        (Lang::Zh, None) => format!("{heading}："),
        (_, Some(at)) => format!("{heading} ({}):", fmt_local_time(at, now)),
        (_, None) => format!("{heading}:"),
    }
}

/// 「… 已省略 N 步」标注（>0 且有可显示步时才有）。
pub fn omitted_line_text(f: &WatchFrame, lang: Lang) -> Option<String> {
    if f.steps_omitted > 0 && !f.steps.is_empty() {
        Some(i18n::tr(lang, "watch.stepsOmitted").replace("{n}", &f.steps_omitted.to_string()))
    } else {
        None
    }
}

/// 「最后更新 14:32:07」。
pub fn updated_line_text(now: u64, lang: Lang) -> String {
    i18n::tr(lang, "watch.updatedAt").replace("{time}", &fmt_local_time(now, now))
}

/// 终态标签（`已结束 · 已自动取消关注` / `已移至最新卡片 ⬇` / …）。
pub fn final_label_text(kind: &FinalKind, lang: Lang) -> String {
    if let FinalKind::AutoStopped(to) = kind {
        return i18n::tr(lang, "watch.btnAutoStopped").replace("{to}", to);
    }
    i18n::tr(
        lang,
        match kind {
            FinalKind::Ended => "watch.btnEnded",
            FinalKind::Cancelled => "watch.btnCancelled",
            FinalKind::Replaced => "watch.btnReplaced",
            FinalKind::Moved => "watch.btnMoved",
            FinalKind::Idle => "watch.btnIdle",
            FinalKind::Rewatched => "watch.btnRewatched",
            FinalKind::AutoStopped(_) => unreachable!("handled above"),
        },
    )
    .to_string()
}

/// 可重新关注终态的按钮文案（可点击，非 disabled）。
pub fn rewatch_label_text(kind: &FinalKind, lang: Lang) -> String {
    match kind {
        FinalKind::AutoStopped(to) => i18n::tr(lang, "watch.btnRewatch").replace("{to}", to),
        FinalKind::Cancelled => i18n::tr(lang, "watch.btnRewatchCancelled").to_string(),
        _ => final_label_text(kind, lang),
    }
}

/// 组装飞书卡片视图（本地化文案在此完成）。`now` 为渲染时刻（Unix 秒，进「最后更新」行）。
/// `session_id`：当 `mode` 为可重新关注终态时需传入以嵌入重新关注按钮的回调数据。
pub fn card_view(
    f: &WatchFrame,
    mode: CardMode,
    now: u64,
    lang: Lang,
    session_id: Option<&str>,
) -> crate::feishu::card::WatchCardView {
    use crate::feishu::card::{WatchButtons, WatchCardView};

    let no_activity = if f.text.is_none() && f.steps.is_empty() {
        Some(i18n::tr(lang, "autoChannel.statusNoActivity").to_string())
    } else {
        None
    };

    let buttons = match mode {
        CardMode::Active => WatchButtons::Active {
            unwatch: i18n::tr(lang, "watch.btnUnwatch").to_string(),
            refresh: i18n::tr(lang, "watch.btnRefresh").to_string(),
        },
        CardMode::Final(ref kind) if kind.is_rewatchable() && session_id.is_some() => {
            WatchButtons::Rewatch {
                label: rewatch_label_text(kind, lang),
                session_id: session_id.unwrap().to_string(),
            }
        }
        CardMode::Final(kind) => WatchButtons::Final {
            label: final_label_text(&kind, lang),
        },
    };

    // 足迹时间线（飞书 markdown 渲染）；「… 已省略 N 步」标注（灰字）置于首行。
    let mut step_lines: Vec<String> = f
        .steps
        .iter()
        .map(|s| render_step_feishu(s, lang))
        .collect();
    if let Some(om) = omitted_line_text(f, lang) {
        step_lines.insert(0, format!("<font color='grey'>{}</font>", om));
    }

    WatchCardView {
        header: header_text(f, lang),
        state_line: state_line_text(f, now, lang),
        title_line: f.title.as_ref().map(|t| format!("「{}」", t)),
        activity_heading: activity_heading_text(f, now, lang),
        text: f.text.clone(),
        step_lines,
        todo_summary: autochannel::todo_summary(&f.todos, lang),
        todo_lines: f.todos.iter().map(render_todo_feishu).collect(),
        no_activity,
        updated_line: updated_line_text(now, lang),
        buttons,
    }
}

/// 飞书卡片 markdown 的足迹步行：彩色圆点（进行中绿 / 已完成灰 / 失败红，`<font>` 标签）+
/// **类别词加粗** + *对象斜体*（用户定案：去类别 emoji，靠粗/斜体区分名字与参数）。
/// 颜色枚举为飞书卡片官方值（green/grey/red）。
fn render_step_feishu(step: &crate::agents::activity::ToolStep, lang: Lang) -> String {
    use crate::agents::activity::StepState;
    let color = match step.state {
        StepState::Running => "green",
        StepState::Done => "grey",
        StepState::Failed => "red",
    };
    let (label, object) = autochannel::step_label_object(step, lang);
    match object {
        Some(o) => format!("<font color='{}'>●</font> **{}**: *{}*", color, label, o),
        None => format!("<font color='{}'>●</font> **{}**", color, label),
    }
}

/// 飞书卡片 markdown 的 TODO 清单行（折叠面板内容）：进行中绿点加粗、已完成灰点删除线、
/// 待办空心圈。cancelled 条目在解析层已剔除。
fn render_todo_feishu(item: &crate::agents::activity::TodoItem) -> String {
    use crate::agents::activity::TodoState;
    match item.state {
        TodoState::InProgress => {
            format!("<font color='green'>●</font> **{}**", item.content)
        }
        TodoState::Completed => {
            format!("<font color='grey'>●</font> ~~{}~~", item.content)
        }
        TodoState::Pending | TodoState::Cancelled => format!("○ {}", item.content),
    }
}

/// 简短时长：`45 秒` / `6 分钟` / `1 小时 20 分`（en：`45s` / `6m` / `1h20m`）。
pub fn fmt_duration(secs: u64, lang: Lang) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    match lang {
        Lang::Zh => {
            if h > 0 {
                format!("{h} 小时 {m} 分")
            } else if m > 0 {
                format!("{m} 分钟")
            } else {
                format!("{s} 秒")
            }
        }
        Lang::En => {
            if h > 0 {
                format!("{h}h{m}m")
            } else if m > 0 {
                format!("{m}m")
            } else {
                format!("{s}s")
            }
        }
    }
}

/// 本地时区绝对时刻：与 `now` 同日 → `HH:MM:SS`；跨日 → `MM-DD HH:MM`。
pub fn fmt_local_time(epoch: u64, now: u64) -> String {
    crate::local_time::compact(epoch, now).unwrap_or_else(|| fmt_utc_time(epoch, now))
}

/// UTC fallback for timestamps outside the maintained timezone backend's range.
fn fmt_utc_time(epoch: u64, now: u64) -> String {
    let (h, m, s) = ((epoch / 3600) % 24, (epoch / 60) % 60, epoch % 60);
    if epoch / 86400 == now / 86400 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        // 无日期分解依赖：跨日退化为「天数差 + 时分」标注（极少走到）。
        format!("{:02}:{:02} UTC", h, m)
    }
}

/// 取工作目录的末段作为项目名（空 → None）。与 `autochannel::project_name` 一致（其为私有）。
fn project_name(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rec(state: &str) -> Value {
        json!({
            "seq": 3,
            "kind": "cursor",
            "sessionId": "no-such-session-watch-test",
            "state": state,
            "title": "重构空闲退出",
            "cwd": "/tmp/HumanInLoop",
        })
    }

    #[test]
    fn frame_phase_derivation() {
        let f = build_frame(3, Some(&rec("working")), false);
        assert_eq!(f.phase, WatchPhase::Working);
        assert_eq!(f.kind_label, "Cursor");
        assert_eq!(f.project.as_deref(), Some("HumanInLoop"));
        // waiting 覆盖 working / idle。
        assert_eq!(
            build_frame(3, Some(&rec("working")), true).phase,
            WatchPhase::Waiting
        );
        assert_eq!(
            build_frame(3, Some(&rec("idle")), true).phase,
            WatchPhase::Waiting
        );
        // ended 不受 waiting 影响。
        assert_eq!(
            build_frame(3, Some(&rec("ended")), true).phase,
            WatchPhase::Ended
        );
        // 记录消失 → Ended 最小帧。
        assert_eq!(build_frame(3, None, false).phase, WatchPhase::Ended);
    }

    #[test]
    fn signature_changes_with_content_only() {
        let a = build_frame(3, Some(&rec("working")), false);
        let b = build_frame(3, Some(&rec("working")), false);
        assert_eq!(signature(&a), signature(&b));
        let c = build_frame(3, Some(&rec("idle")), false);
        assert_ne!(signature(&a), signature(&c));
        let d = build_frame(3, Some(&rec("working")), true);
        assert_ne!(signature(&a), signature(&d));
    }

    #[test]
    fn signature_ignores_activity_timestamp() {
        // 活动时刻走动（transcript mtime / 工具心跳）但内容不变 → 签名不变，不触发卡片编辑。
        let mut a = build_frame(3, Some(&rec("working")), false);
        let mut b = a.clone();
        a.at = Some(1_700_000_000);
        b.at = Some(1_700_000_555);
        assert_eq!(signature(&a), signature(&b));
        // 内容变化仍触发。
        b.text = Some("新输出".into());
        assert_ne!(signature(&a), signature(&b));
    }

    #[test]
    fn lineage_is_visible_and_part_of_the_card_signature() {
        let mut child = rec("working");
        child["forkedFromSessionId"] = Value::String("parent-session-123".into());
        let frame = build_frame(3, Some(&child), false);
        assert_eq!(frame.forked_from.as_deref(), Some("parent-s"));
        assert_eq!(
            state_line_text(&frame, 1_700_000_000, Lang::Zh),
            "🟢 工作中 · 从 parent-s 分叉"
        );
        let without_parent = build_frame(3, Some(&rec("working")), false);
        assert_ne!(signature(&frame), signature(&without_parent));
    }

    #[test]
    fn card_view_localizes_and_maps_buttons() {
        let f = build_frame(3, Some(&rec("working")), false);
        let now = 1_700_000_000;
        let v = card_view(&f, CardMode::Active, now, Lang::Zh, None);
        assert!(v.header.contains("[3]"));
        assert!(v.header.contains("Cursor"));
        assert!(v.header.contains("HumanInLoop"));
        assert_eq!(v.state_line, "🟢 工作中");
        assert_eq!(v.title_line.as_deref(), Some("「重构空闲退出」"));
        // 该 session 无 transcript → 「暂无活动」占位。
        assert!(v.no_activity.is_some());
        match v.buttons {
            crate::feishu::card::WatchButtons::Active {
                ref unwatch,
                ref refresh,
            } => {
                assert_eq!(unwatch, "取消关注");
                assert_eq!(refresh, "立即刷新");
            }
            _ => panic!("expected active buttons"),
        }
        // 终态：单个禁用按钮 + 对应文案。
        let fin = card_view(
            &f,
            CardMode::Final(FinalKind::Cancelled),
            now,
            Lang::Zh,
            None,
        );
        match fin.buttons {
            crate::feishu::card::WatchButtons::Final { ref label } => {
                assert_eq!(label, "已取消关注")
            }
            _ => panic!("expected final button"),
        }
    }

    #[test]
    fn auto_stopped_label_is_dynamic() {
        // 「自动结束 watch」终态：动态文案「已切换到 {to} · 自动结束关注」。
        let kind = FinalKind::AutoStopped("本地弹窗".to_string());
        assert_eq!(
            final_label_text(&kind, Lang::Zh),
            "已切换到 本地弹窗 · 自动结束关注"
        );
        assert_eq!(
            final_label_text(&kind, Lang::En),
            "Auto-stopped (switched to 本地弹窗)"
        );
    }

    #[test]
    fn idle_label_text() {
        assert_eq!(
            final_label_text(&FinalKind::Idle, Lang::Zh),
            "已空闲 · 已自动取消关注"
        );
        assert_eq!(
            final_label_text(&FinalKind::Idle, Lang::En),
            "Idle · auto-unwatched"
        );
    }

    #[test]
    fn stats_line_shows_cumulative_active_time_in_all_phases() {
        let now = 1_700_000_000u64;
        let mut r = rec("working");
        r["activeElapsedSecs"] = json!(6 * 60);
        let f = build_frame(3, Some(&r), false);
        let v = card_view(&f, CardMode::Active, now, Lang::Zh, None);
        assert_eq!(v.state_line, "🟢 工作中 · 累计工作 6 分钟");
        // Cumulative time under one minute remains hidden on live Watch cards.
        let mut r2 = rec("working");
        r2["activeElapsedSecs"] = json!(30);
        let f2 = build_frame(3, Some(&r2), false);
        assert_eq!(
            card_view(&f2, CardMode::Active, now, Lang::Zh, None).state_line,
            "🟢 工作中"
        );
        // Idle and Ended cards retain the frozen cumulative total.
        let mut r4 = rec("idle");
        r4["activeElapsedSecs"] = json!(600);
        let f4 = build_frame(3, Some(&r4), false);
        assert_eq!(
            card_view(&f4, CardMode::Active, now, Lang::Zh, None).state_line,
            "⚪ 空闲 · 累计工作 10 分钟"
        );
        let mut r5 = rec("ended");
        r5["activeElapsedSecs"] = json!(600);
        let f5 = build_frame(3, Some(&r5), false);
        assert_eq!(
            card_view(&f5, CardMode::Active, now, Lang::Zh, None).state_line,
            "⏹ 已结束 · 累计工作 10 分钟"
        );
        // The cumulative clock is excluded from the signature and cannot trigger edits alone.
        assert_eq!(
            signature(&f),
            signature(&{
                let mut r6 = rec("working");
                r6["activeElapsedSecs"] = json!(7 * 60);
                build_frame(3, Some(&r6), false)
            })
        );
    }

    #[test]
    fn feishu_step_line_dot_bold_italic() {
        use crate::agents::activity::{StepState, ToolDisplay, ToolLabel, ToolStep};
        let step = |state: StepState| ToolStep {
            tool: ToolDisplay {
                label: ToolLabel::Run,
                object: Some("cargo test".into()),
            },
            state,
        };
        assert_eq!(
            render_step_feishu(&step(StepState::Running), Lang::Zh),
            "<font color='green'>●</font> **运行命令**: *cargo test*"
        );
        assert_eq!(
            render_step_feishu(&step(StepState::Done), Lang::Zh),
            "<font color='grey'>●</font> **运行命令**: *cargo test*"
        );
        assert!(render_step_feishu(&step(StepState::Failed), Lang::Zh).contains("color='red'"));
        // 无对象：只有加粗类别词。
        let bare = ToolStep {
            tool: ToolDisplay {
                label: ToolLabel::Other("Grep".into()),
                object: None,
            },
            state: StepState::Done,
        };
        assert_eq!(
            render_step_feishu(&bare, Lang::Zh),
            "<font color='grey'>●</font> **Grep**"
        );
    }

    #[test]
    fn persisted_roundtrip() {
        let items = vec![PersistedWatch {
            channel: "feishu".into(),
            session_id: "s1".into(),
            message_id: "om_1".into(),
            created_at: 42,
            rewatchable: false,
        }];
        let text = serde_json::to_string(&items).unwrap();
        let back: Vec<PersistedWatch> = serde_json::from_str(&text).unwrap();
        assert_eq!(back, items);
        // camelCase 字段名（与其它持久化文件一致）。
        assert!(text.contains("sessionId"));
        assert!(text.contains("messageId"));
    }

    #[test]
    fn idle_grace_expires_after_five_minutes_only_for_idle_records() {
        let now = 10_000;
        let idle = |last_activity| {
            serde_json::json!({
                "state": "idle",
                "lastActivity": last_activity,
            })
        };
        assert!(!idle_grace_expired(
            Some(&idle(now - IDLE_GRACE_SECS + 1)),
            now
        ));
        assert!(idle_grace_expired(Some(&idle(now - IDLE_GRACE_SECS)), now));
        assert!(!idle_grace_expired(
            Some(&serde_json::json!({
                "state": "working",
                "lastActivity": now - IDLE_GRACE_SECS,
            })),
            now
        ));
        assert!(!idle_grace_expired(None, now));
        assert!(idle_grace_expired(
            Some(&serde_json::json!({ "state": "idle" })),
            now
        ));
    }

    #[test]
    fn existing_watch_resumes_within_idle_grace_and_ends_at_deadline() {
        let idle_since = 1_000;
        let idle = serde_json::json!({
            "seq": 1,
            "kind": "codex",
            "sessionId": "s1",
            "state": "idle",
            "lastActivity": idle_since,
        });
        let idle_frame = build_frame(1, Some(&idle), false);
        assert_eq!(
            automatic_final_kind(&idle_frame, Some(&idle), idle_since + IDLE_GRACE_SECS - 1),
            None
        );

        let mut working = idle.clone();
        working["state"] = serde_json::json!("working");
        let working_frame = build_frame(1, Some(&working), false);
        assert_eq!(
            automatic_final_kind(&working_frame, Some(&working), idle_since + IDLE_GRACE_SECS),
            None
        );

        assert_eq!(
            automatic_final_kind(&idle_frame, Some(&idle), idle_since + IDLE_GRACE_SECS),
            Some(FinalKind::Idle)
        );
        let waiting_frame = build_frame(1, Some(&idle), true);
        assert_eq!(
            automatic_final_kind(&waiting_frame, Some(&idle), idle_since + IDLE_GRACE_SECS),
            None
        );

        let mut ended = idle.clone();
        ended["state"] = serde_json::json!("ended");
        let ended_frame = build_frame(1, Some(&ended), false);
        assert_eq!(
            automatic_final_kind(&ended_frame, Some(&ended), idle_since + 1),
            Some(FinalKind::Ended)
        );
    }

    #[test]
    fn idle_grace_deadline_survives_snapshot_roundtrip() {
        let idle_since = 2_000;
        let rec = serde_json::json!({
            "state": "idle",
            "lastActivity": idle_since,
        });
        let restored: Value = serde_json::from_str(&serde_json::to_string(&rec).unwrap()).unwrap();
        assert!(!idle_grace_expired(
            Some(&restored),
            idle_since + IDLE_GRACE_SECS - 1
        ));
        assert!(idle_grace_expired(
            Some(&restored),
            idle_since + IDLE_GRACE_SECS
        ));
    }

    #[test]
    fn local_time_same_day_vs_cross_day() {
        // 同一时刻恒为 HH:MM:SS 形态（本地时区不定，只验形态）。
        let now = 1_700_000_000u64;
        let s = fmt_local_time(now, now);
        assert_eq!(s.len(), 8);
        assert_eq!(s.as_bytes()[2], b':');
        // 跨日：含日期段（MM-DD HH:MM）。
        let old = now - 3 * 86400;
        let s2 = fmt_local_time(old, now);
        assert!(s2.contains('-') || s2.contains("UTC"));
    }
}
