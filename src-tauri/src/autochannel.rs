//! 「IM 会话期自动激活」的与传输无关的小逻辑：活跃槽持久化、入站 slash 命令解析、
//! `/status` 文本组装、激活回执文案。
//!
//! 设计见 `docs/plans/im-channel-activation.md`。活跃槽（当前用哪个 IM 接收提问）持久化到
//! `~/.askhuman/state/auto-channel.json`，跨 daemon 重启保留，仅由「用户在某渠道的入站消息」更新。

use crate::i18n::{self, Lang};
use crate::paths;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 持久化的活跃槽。
#[derive(Default, Serialize, Deserialize)]
struct Persisted {
    /// 当前活跃渠道 id（"feishu" / "dingding" / "telegram" / "slack" / "popup"）；
    /// None / "popup" = 不向任何 IM 发卡片（只弹窗）。在哪个渠道作答 / 说话就更新为哪个。
    #[serde(default)]
    channel: Option<String>,
    /// 最近一次更新时间（unix 秒，仅诊断用）。
    #[serde(default)]
    updated_at: u64,
}

/// 读取持久化的活跃槽（缺失 / 解析失败 → None）。
pub fn load_active() -> Option<String> {
    let text = std::fs::read_to_string(paths::auto_channel_file()).ok()?;
    let parsed: Persisted = serde_json::from_str(&text).ok()?;
    parsed.channel.filter(|s| !s.is_empty())
}

/// 原子写入活跃槽（临时文件 + rename）。best-effort，失败静默。
pub fn save_active(channel: Option<&str>) {
    let data = Persisted {
        channel: channel.map(|s| s.to_string()),
        updated_at: now_secs(),
    };
    let Ok(json) = serde_json::to_string_pretty(&data) else {
        return;
    };
    let path = paths::auto_channel_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("json.tmp-{}", uuid::Uuid::new_v4()));
    if std::fs::write(&tmp, json.as_bytes()).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// 入站内置命令（带 `/` 前缀才算命令；可扩展）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `/new`、`/新任务`：从 IM 选择工作区和 Agent，并在电脑上打开可接续的终端任务。
    /// 带任何参数均由调用方作为用法错误处理。
    New { has_args: bool },
    /// `/fork [编号]`：从活动/空闲原生会话立即分叉；除可选编号外不接受内联指令。
    Fork {
        sel: Option<u64>,
        has_invalid_args: bool,
    },
    /// `/here`、`/这里`：把此渠道设为活跃槽 + 补推在途 + 必回执。
    Here,
    /// `/status`、`/状态`：`None` 返回工作中/空闲 agent 列表；`Some(编号)` 返回该 agent 的当前活动详情。
    Status(Option<u64>),
    /// `/watch`、`/关注`：`Some(编号)` 关注该 agent（发实时状态卡）；`None` 列出当前关注。
    Watch(Option<u64>),
    /// `/unwatch`、`/取消关注`：取消关注（编号 / 全部 / 缺省自动）。
    Unwatch(WatchSel),
    /// `/msg <编号> <内容>`、`/插话`：给该 agent 排队一条插话（spec agent-interject D2/D9）。
    /// 有编号无内容 → 回显当前待送达全文；**无编号有内容** → 自动选择目标（关注恰 1 个且工作中直发，
    /// 否则弹选择卡，见 `docs/plans/im-msg-select-card.md`）；无编号无内容 → 回增强用法提示。
    /// 内容保留原始换行（多行插话原样送达）。
    Msg(Option<u64>, Option<String>),
    /// `/msg-clear <编号>`、`/撤回`：清空该 agent 的待送达插话。编号缺省 → 回用法提示。
    MsgClear(Option<u64>),
    /// `/diff [编号]`：导出 agent 工作区未暂存 diff（无参 → 单选卡）。
    Diff(Option<u64>),
    /// `/stage [编号]`：确认后 stage 未暂存改动（无参 → 单选卡）。
    Stage(Option<u64>),
    /// `/transcript [编号]`：导出 agent 完整会话渲染（无参 → 单选卡）。
    Transcript(Option<u64>),
    /// `/todo`、`/待办`：无编号 → 选项目（带文本则选中后新增）；`Some(n)` 是兼容入口，
    /// 无文本打开 Agent n 所在项目的管理卡，带文本直接追加。文本保留原始换行。
    Todo(Option<u64>, Option<String>),
    /// `/todo-rm`、`/删待办`：无参 → 选项目；`Some(n)` 兼容 Agent 编号直达。
    TodoRm(Option<u64>),
    /// `/todo-auto`、`/自动待办`：无编号 → 选项目（带文本则新增自动待办）；`Some(n)` 是
    /// 兼容入口，无文本打开切换卡，带文本直接新增一条自动执行待办。
    TodoAuto(Option<u64>, Option<String>),
    /// `/yolo [off [编号]]`：Codex YOLO 模式管理（D53）。`List` → 带关闭按钮的会话选择卡；
    /// `Off(Some(n))` → 按 Agent 编号直关；`Off(None)` → 恰一个开着直关，多个弹卡。
    Yolo(YoloSel),
    /// `/help`、`/帮助`、`/?`：返回动态引导文案（可发什么、可用命令）。
    Help,
}

/// `/yolo` 的子命令。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum YoloSel {
    /// 无参：列出开启中的会话（带关闭按钮的选择卡）。
    List,
    /// `off [编号]`：关闭（编号 = /status 的 Agent 编号；缺省时恰一个开着则直关）。
    Off(Option<u64>),
}

/// `/unwatch` 的目标选择。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchSel {
    /// 指定编号。
    One(u64),
    /// 全部（`all` / `全部`）。
    All,
    /// 未指定：恰一个关注则取消它，多个则回列表让用户指定。
    Auto,
}

/// 一条入站文本的分类（供 `handle_inbound` 分派）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// 已识别的内置命令。
    Command(Command),
    /// 以 `/` 开头但不认识的命令（armed 时不会进卡片当答案 → 安全回引导）。
    UnknownCommand,
    /// 非 `/` 开头的普通文本（可能被当作答案）。
    Text,
}

/// 无斜线前缀时的整句短语 → 无参命令（spec `docs/specs/im-command-phrases.md`）。
/// 键已是 [`crate::textnorm::normalize_key`] 结果；表构建时要求键唯一。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PhraseKind {
    New,
    Fork,
    Here,
    Help,
    Status,
    Watch,
    Unwatch,
    Msg,
    Diff,
    Stage,
    Transcript,
    Todo,
    TodoRm,
    TodoAuto,
    Yolo,
}

/// (normalized key, command). Includes slash command names, Chinese token aliases, and
/// longer voice-friendly phrases. No-arg forms only.
const COMMAND_PHRASES: &[(&str, PhraseKind)] = &[
    // /new
    ("new", PhraseKind::New),
    ("新建", PhraseKind::New),
    ("新建会话", PhraseKind::New),
    ("新建任务", PhraseKind::New),
    ("新任务", PhraseKind::New),
    ("开新任务", PhraseKind::New),
    ("开始新任务", PhraseKind::New),
    ("新建agent", PhraseKind::New),
    ("新建代理", PhraseKind::New),
    ("开启新任务", PhraseKind::New),
    ("newsession", PhraseKind::New),
    ("newtask", PhraseKind::New),
    ("newagent", PhraseKind::New),
    ("startnewtask", PhraseKind::New),
    ("createnewsession", PhraseKind::New),
    ("createnewtask", PhraseKind::New),
    ("startanewtask", PhraseKind::New),
    // /fork
    ("fork", PhraseKind::Fork),
    ("分叉", PhraseKind::Fork),
    ("分叉会话", PhraseKind::Fork),
    ("会话分叉", PhraseKind::Fork),
    ("forksession", PhraseKind::Fork),
    // /here
    ("here", PhraseKind::Here),
    ("这里", PhraseKind::Here),
    ("就在这里", PhraseKind::Here),
    ("用这里", PhraseKind::Here),
    ("切换到这里", PhraseKind::Here),
    ("在这里回复", PhraseKind::Here),
    ("righthere", PhraseKind::Here),
    ("usethis", PhraseKind::Here),
    ("usethischannel", PhraseKind::Here),
    ("replyhere", PhraseKind::Here),
    // /help
    ("help", PhraseKind::Help),
    ("帮助", PhraseKind::Help),
    ("怎么用", PhraseKind::Help),
    ("有哪些命令", PhraseKind::Help),
    ("命令列表", PhraseKind::Help),
    ("使用帮助", PhraseKind::Help),
    ("commands", PhraseKind::Help),
    ("whatcanido", PhraseKind::Help),
    ("showhelp", PhraseKind::Help),
    ("listcommands", PhraseKind::Help),
    // /status
    ("status", PhraseKind::Status),
    ("状态", PhraseKind::Status),
    ("当前状态", PhraseKind::Status),
    ("agent状态", PhraseKind::Status),
    ("看看状态", PhraseKind::Status),
    ("查看状态", PhraseKind::Status),
    ("agentstatus", PhraseKind::Status),
    ("showstatus", PhraseKind::Status),
    ("listagents", PhraseKind::Status),
    // /watch
    ("watch", PhraseKind::Watch),
    ("关注", PhraseKind::Watch),
    ("开始关注", PhraseKind::Watch),
    ("startwatch", PhraseKind::Watch),
    // /unwatch
    ("unwatch", PhraseKind::Unwatch),
    ("取消关注", PhraseKind::Unwatch),
    ("停止关注", PhraseKind::Unwatch),
    ("stopwatch", PhraseKind::Unwatch),
    ("endwatch", PhraseKind::Unwatch),
    // /msg
    ("msg", PhraseKind::Msg),
    ("插话", PhraseKind::Msg),
    ("发消息", PhraseKind::Msg),
    ("给agent发消息", PhraseKind::Msg),
    ("发送消息", PhraseKind::Msg),
    ("message", PhraseKind::Msg),
    ("sendmessage", PhraseKind::Msg),
    ("interject", PhraseKind::Msg),
    // /diff
    ("diff", PhraseKind::Diff),
    ("查看diff", PhraseKind::Diff),
    ("看diff", PhraseKind::Diff),
    ("变更", PhraseKind::Diff),
    ("查看变更", PhraseKind::Diff),
    ("showdiff", PhraseKind::Diff),
    ("viewdiff", PhraseKind::Diff),
    // /stage
    ("stage", PhraseKind::Stage),
    ("暂存", PhraseKind::Stage),
    ("全部暂存", PhraseKind::Stage),
    ("stageall", PhraseKind::Stage),
    ("gitadd", PhraseKind::Stage),
    // /transcript
    ("transcript", PhraseKind::Transcript),
    ("导出会话", PhraseKind::Transcript),
    ("导出记录", PhraseKind::Transcript),
    ("会话记录", PhraseKind::Transcript),
    ("exporttranscript", PhraseKind::Transcript),
    ("exportsession", PhraseKind::Transcript),
    // /todo
    ("todo", PhraseKind::Todo),
    ("待办", PhraseKind::Todo),
    ("待办列表", PhraseKind::Todo),
    ("查看待办", PhraseKind::Todo),
    ("打开待办", PhraseKind::Todo),
    ("todos", PhraseKind::Todo),
    ("todolist", PhraseKind::Todo),
    ("showtodos", PhraseKind::Todo),
    // /todo-rm
    ("todorm", PhraseKind::TodoRm),
    ("删待办", PhraseKind::TodoRm),
    ("删除待办", PhraseKind::TodoRm),
    ("deletetodo", PhraseKind::TodoRm),
    ("removetodo", PhraseKind::TodoRm),
    // /todo-auto
    ("todoauto", PhraseKind::TodoAuto),
    ("自动待办", PhraseKind::TodoAuto),
    ("autotodo", PhraseKind::TodoAuto),
    // /yolo（一律指向列表卡：卡上自带关闭按钮，"yolo off"/"关闭yolo" 规范化后也命中）
    ("yolo", PhraseKind::Yolo),
    ("yolo模式", PhraseKind::Yolo),
    ("关闭yolo", PhraseKind::Yolo),
    ("关yolo", PhraseKind::Yolo),
    ("yolooff", PhraseKind::Yolo),
    ("yolomode", PhraseKind::Yolo),
    ("turnoffyolo", PhraseKind::Yolo),
];

fn phrase_kind_to_command(kind: PhraseKind) -> Command {
    match kind {
        PhraseKind::New => Command::New { has_args: false },
        PhraseKind::Fork => Command::Fork {
            sel: None,
            has_invalid_args: false,
        },
        PhraseKind::Here => Command::Here,
        PhraseKind::Help => Command::Help,
        PhraseKind::Status => Command::Status(None),
        PhraseKind::Watch => Command::Watch(None),
        PhraseKind::Unwatch => Command::Unwatch(WatchSel::Auto),
        PhraseKind::Msg => Command::Msg(None, None),
        PhraseKind::Diff => Command::Diff(None),
        PhraseKind::Stage => Command::Stage(None),
        PhraseKind::Transcript => Command::Transcript(None),
        PhraseKind::Todo => Command::Todo(None, None),
        PhraseKind::TodoRm => Command::TodoRm(None),
        PhraseKind::TodoAuto => Command::TodoAuto(None, None),
        PhraseKind::Yolo => Command::Yolo(YoloSel::List),
    }
}

/// Match a bare (no `/`/`!`) inbound message against the command phrase table.
fn match_command_phrase(text: &str) -> Option<Command> {
    let key = crate::textnorm::normalize_key(text);
    if key.is_empty() {
        return None;
    }
    COMMAND_PHRASES
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, kind)| phrase_kind_to_command(*kind))
}

/// 解析入站文本。
///
/// 1. **`/` 或 `!` 前缀**：现有 slash 命令分派（见下）。
/// 2. **无前缀整句短语**：规范化后命中词表 → 无参命令（spec im-command-phrases；与 slash 同优先于作答）。
/// 3. 否则 → `Text`。
///
/// `!` 是备用前缀（B 案，四渠道通用）：Slack 客户端把**一切** `/` 开头的输入拦截为
/// slash command 在本地解析，未注册的名字根本发不出来——`!status`/`!watch 3` 是普通消息，畅通。
/// 两个前缀的**未知命令**语义不同：`/xxx` 依约定必是命令 → `UnknownCommand`（回引导）；
/// `!xxx` 未匹配则视为普通文本（`Text`）——`!` 开头的自由回答（如 "!important"）不能被劫持。
///
/// `/status <编号>`：第二个 token 是纯数字则解析为编号（`Some`），缺省 / 非数字则 `None`（全局列表）。
pub fn classify(text: &str) -> Parsed {
    let trimmed = text.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('!') {
        return classify_prefixed(trimmed);
    }
    if let Some(cmd) = match_command_phrase(trimmed) {
        return Parsed::Command(cmd);
    }
    Parsed::Text
}

fn classify_prefixed(trimmed: &str) -> Parsed {
    let (bare, bang) = match trimmed.strip_prefix('/') {
        Some(rest) => (rest, false),
        None => match trimmed.strip_prefix('!') {
            Some(rest) => (rest, true),
            None => return Parsed::Text,
        },
    };
    // 首 token（命令名）与其后的原文剩余。`/msg` 的内容需保留原始换行 / 空白结构，
    // 不能整句 split_whitespace 后重拼，故此处手工切分。
    let bare = bare.trim_start();
    let (token, rest) = match bare.find(char::is_whitespace) {
        Some(i) => (&bare[..i], bare[i..].trim_start()),
        None => (bare, ""),
    };
    let mut tokens = rest.split_whitespace();
    match token.to_ascii_lowercase().as_str() {
        "new" | "新任务" => Parsed::Command(Command::New {
            has_args: !rest.trim().is_empty(),
        }),
        "fork" | "分叉" => {
            let values: Vec<&str> = tokens.collect();
            let sel = values.first().and_then(|value| value.parse::<u64>().ok());
            Parsed::Command(Command::Fork {
                sel,
                has_invalid_args: values.len() > 1 || (values.len() == 1 && sel.is_none()),
            })
        }
        "here" | "这里" => Parsed::Command(Command::Here),
        "status" | "状态" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::Status(sel))
        }
        "watch" | "关注" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::Watch(sel))
        }
        "unwatch" | "取消关注" => {
            let sel = match tokens.next() {
                Some(t) if t.eq_ignore_ascii_case("all") || t == "全部" => WatchSel::All,
                Some(t) => match t.parse::<u64>() {
                    Ok(n) => WatchSel::One(n),
                    Err(_) => WatchSel::Auto,
                },
                None => WatchSel::Auto,
            };
            Parsed::Command(Command::Unwatch(sel))
        }
        "msg" | "插话" => {
            // 首 token 为纯数字 → 编号 + 其后原文（含换行）为内容；首 token 非数字 → 整段 rest 作为内容
            // （无编号，交自动选择流程：关注恰 1 个且工作中直发，否则弹选择卡）；空 → (None, None)。
            let (first, content) = match rest.find(char::is_whitespace) {
                Some(i) => (&rest[..i], rest[i..].trim_start()),
                None => (rest, ""),
            };
            match first.parse::<u64>() {
                Ok(n) => {
                    let content = (!content.is_empty()).then(|| content.to_string());
                    Parsed::Command(Command::Msg(Some(n), content))
                }
                Err(_) => {
                    let content = (!rest.is_empty()).then(|| rest.to_string());
                    Parsed::Command(Command::Msg(None, content))
                }
            }
        }
        "msg-clear" | "撤回" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::MsgClear(sel))
        }
        "diff" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::Diff(sel))
        }
        "stage" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::Stage(sel))
        }
        "transcript" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::Transcript(sel))
        }
        "todo" | "待办" => {
            // 与 `/msg` 同构：首 token 为纯数字 → 编号 + 其后原文（含换行）为待办文本；
            // 首 token 非数字 → 无编号 + 整段 rest 作文本（先选项目再新增）；空 → 项目管理入口。
            let (first, content) = match rest.find(char::is_whitespace) {
                Some(i) => (&rest[..i], rest[i..].trim_start()),
                None => (rest, ""),
            };
            match first.parse::<u64>() {
                Ok(n) => {
                    let content = (!content.is_empty()).then(|| content.to_string());
                    Parsed::Command(Command::Todo(Some(n), content))
                }
                Err(_) => {
                    let content = (!rest.is_empty()).then(|| rest.to_string());
                    Parsed::Command(Command::Todo(None, content))
                }
            }
        }
        "todo-rm" | "删待办" => {
            let sel = tokens.next().and_then(|s| s.parse::<u64>().ok());
            Parsed::Command(Command::TodoRm(sel))
        }
        "todo-auto" | "自动待办" => {
            // 语法同 /todo：`<n> [text]` 保留 Agent 编号兼容；无编号时先选项目。
            let (first, content) = match rest.find(char::is_whitespace) {
                Some(i) => (&rest[..i], rest[i..].trim_start()),
                None => (rest, ""),
            };
            match first.parse::<u64>() {
                Ok(n) => {
                    let content = (!content.is_empty()).then(|| content.to_string());
                    Parsed::Command(Command::TodoAuto(Some(n), content))
                }
                Err(_) => {
                    let content = (!rest.is_empty()).then(|| rest.to_string());
                    Parsed::Command(Command::TodoAuto(None, content))
                }
            }
        }
        "yolo" => {
            let sel = match tokens.next() {
                Some(sub) if sub.eq_ignore_ascii_case("off") || sub == "关闭" || sub == "关" => {
                    YoloSel::Off(tokens.next().and_then(|value| value.parse::<u64>().ok()))
                }
                _ => YoloSel::List,
            };
            Parsed::Command(Command::Yolo(sel))
        }
        "help" | "帮助" | "?" | "？" => Parsed::Command(Command::Help),
        _ if bang => Parsed::Text,
        _ => Parsed::UnknownCommand,
    }
}

/// 作答内容被接受时的回执种类 / 模式（决定确认文案）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckKind {
    Text,
    Image,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckMode {
    /// 卡片模式：内容累积进答案，需点「提交」定稿。
    Card,
    /// 文本兜底模式：一条消息即完成该题。
    Fallback,
}

/// 「内容被接受进答案」的即时确认文案（spec R2）。仅在内容确实被接受时由渠道会话发送。
pub fn answer_ack_text(kind: AckKind, mode: AckMode, lang: Lang) -> String {
    let key = match (mode, kind) {
        (AckMode::Card, AckKind::Image) => "autoChannel.ackImageCard",
        (AckMode::Card, AckKind::File) => "autoChannel.ackFileCard",
        (AckMode::Card, AckKind::Text) => "autoChannel.ackTextCard",
        (AckMode::Fallback, AckKind::Image) => "autoChannel.ackImageFallback",
        (AckMode::Fallback, AckKind::File) => "autoChannel.ackFileFallback",
        (AckMode::Fallback, AckKind::Text) => "autoChannel.ackTextFallback",
    };
    i18n::tr(lang, key).to_string()
}

/// 自动识别 ID 成功后的回执文案（spec R5）：只报字段名、不回显 ID。
pub fn detect_ack_text(field_label: &str, lang: Lang) -> String {
    i18n::tr(lang, "autoChannel.detectAck").replace("{field}", field_label)
}

/// 渠道展示用的命令前缀：Slack 客户端把一切 `/` 开头输入拦截为 slash command（未注册发不出来），
/// 故 Slack 的提示/引导展示 `!` 前缀；其余渠道维持 `/`。解析侧两个前缀四渠道通用（见 `classify`）。
pub fn cmd_prefix(channel_id: &str) -> &'static str {
    if channel_id == "slack" {
        "!"
    } else {
        "/"
    }
}

/// A channel-neutral help payload. Renderers add Markdown, HTML, cards, and list markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpView {
    pub title: String,
    pub intro: String,
    pub sections: Vec<HelpSection>,
    pub question_state: HelpQuestionState,
    pub switch_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpSection {
    pub title: String,
    pub commands: Vec<HelpCommand>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpCommand {
    pub syntax: String,
    pub description: String,
    pub phrase: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpQuestionState {
    Active { instruction: String },
    None { message: String },
}

pub fn help_phrase_hint(phrase: &str, lang: Lang) -> String {
    i18n::tr(lang, "autoChannel.helpPhraseHint").replace("{phrase}", phrase)
}

/// Build dynamic `/help` content without channel-specific formatting.
///
/// - `auto`: controls `/here` and the channel-switch hint.
/// - `has_active_question`: selects answering guidance vs. the idle state.
/// - `watch`: controls both `/watch` and `/unwatch`.
/// - `prefix`: Slack uses `!`; other channels use `/`.
pub fn help_view(
    auto: bool,
    has_active_question: bool,
    watch: bool,
    prefix: &str,
    lang: Lang,
) -> HelpView {
    let command = |name: &str,
                   en_args: &str,
                   zh_args: &str,
                   description_key: &'static str,
                   phrase_key: Option<&'static str>| {
        let args = match lang {
            Lang::En => en_args,
            Lang::Zh => zh_args,
        };
        HelpCommand {
            syntax: format!("{prefix}{name}{args}"),
            description: i18n::tr(lang, description_key).to_string(),
            phrase: phrase_key.map(|key| i18n::tr(lang, key).to_string()),
        }
    };

    let mut agent_commands = vec![
        command(
            "status",
            " [n]",
            " [编号]",
            "autoChannel.helpDescStatus",
            Some("autoChannel.helpPhraseStatus"),
        ),
        command(
            "new",
            "",
            "",
            "autoChannel.helpDescNew",
            Some("autoChannel.helpPhraseNew"),
        ),
        command(
            "fork",
            " [n]",
            " [编号]",
            "autoChannel.helpDescFork",
            Some("autoChannel.helpPhraseFork"),
        ),
    ];
    if watch {
        agent_commands.push(command(
            "watch",
            " [n]",
            " [编号]",
            "autoChannel.helpDescWatch",
            Some("autoChannel.helpPhraseWatch"),
        ));
        agent_commands.push(command(
            "unwatch",
            " [n|all]",
            " [编号|all]",
            "autoChannel.helpDescUnwatch",
            Some("autoChannel.helpPhraseUnwatch"),
        ));
    }
    agent_commands.extend([
        command(
            "msg",
            " [n] [text]",
            " [编号] [内容]",
            "autoChannel.helpDescMsg",
            Some("autoChannel.helpPhraseMsg"),
        ),
        command(
            "msg-clear",
            " <n>",
            " <编号>",
            "autoChannel.helpDescMsgClear",
            None,
        ),
        command(
            "yolo",
            " [off [n]]",
            " [off [编号]]",
            "autoChannel.helpDescYolo",
            Some("autoChannel.helpPhraseYolo"),
        ),
    ]);

    let code_commands = vec![
        command(
            "diff",
            " [n]",
            " [编号]",
            "autoChannel.helpDescDiff",
            Some("autoChannel.helpPhraseDiff"),
        ),
        command(
            "stage",
            " [n]",
            " [编号]",
            "autoChannel.helpDescStage",
            Some("autoChannel.helpPhraseStage"),
        ),
        command(
            "transcript",
            " [n]",
            " [编号]",
            "autoChannel.helpDescTranscript",
            Some("autoChannel.helpPhraseTranscript"),
        ),
    ];

    let todo_commands = vec![
        command(
            "todo",
            " [text]",
            " [内容]",
            "autoChannel.helpDescTodo",
            Some("autoChannel.helpPhraseTodo"),
        ),
        command(
            "todo-rm",
            "",
            "",
            "autoChannel.helpDescTodoRm",
            Some("autoChannel.helpPhraseTodoRm"),
        ),
        command(
            "todo-auto",
            " [text]",
            " [内容]",
            "autoChannel.helpDescTodoAuto",
            Some("autoChannel.helpPhraseTodoAuto"),
        ),
    ];

    let mut channel_commands = Vec::new();
    if auto {
        channel_commands.push(command(
            "here",
            "",
            "",
            "autoChannel.helpDescHere",
            Some("autoChannel.helpPhraseHere"),
        ));
    }
    channel_commands.push(command(
        "help",
        "",
        "",
        "autoChannel.helpDescHelp",
        Some("autoChannel.helpPhraseHelp"),
    ));

    HelpView {
        title: i18n::tr(lang, "autoChannel.helpTitle").to_string(),
        intro: i18n::tr(lang, "autoChannel.helpIntro").to_string(),
        sections: vec![
            HelpSection {
                title: i18n::tr(lang, "autoChannel.helpGroupAgent").to_string(),
                commands: agent_commands,
            },
            HelpSection {
                title: i18n::tr(lang, "autoChannel.helpGroupCode").to_string(),
                commands: code_commands,
            },
            HelpSection {
                title: i18n::tr(lang, "autoChannel.helpGroupTodo").to_string(),
                commands: todo_commands,
            },
            HelpSection {
                title: i18n::tr(lang, "autoChannel.helpGroupChannel").to_string(),
                commands: channel_commands,
            },
        ],
        question_state: if has_active_question {
            HelpQuestionState::Active {
                instruction: i18n::tr(lang, "autoChannel.helpAnswering").to_string(),
            }
        } else {
            HelpQuestionState::None {
                message: i18n::tr(lang, "autoChannel.helpNoQuestion").to_string(),
            }
        },
        switch_hint: auto.then(|| i18n::tr(lang, "autoChannel.helpSwitchHint").to_string()),
    }
}

/// Render a markup-free grouped fallback for channels that reject the rich payload.
pub fn render_help_plain(view: &HelpView, lang: Lang) -> String {
    let mut out = format!("{}\n{}", view.title, view.intro);
    for section in &view.sections {
        out.push_str("\n\n");
        out.push_str(&section.title);
        for command in &section.commands {
            out.push_str("\n• ");
            out.push_str(&command.syntax);
            out.push_str(" — ");
            out.push_str(&command.description);
            if let Some(phrase) = &command.phrase {
                out.push_str("  · ");
                out.push_str(&help_phrase_hint(phrase, lang));
            }
        }
    }
    out.push_str("\n\n——\n");
    match &view.question_state {
        HelpQuestionState::Active { instruction } => out.push_str(instruction),
        HelpQuestionState::None { message } => out.push_str(message),
    }
    if let Some(hint) = &view.switch_hint {
        out.push('\n');
        out.push_str(hint);
    }
    out
}

/// Backward-compatible plain help wrapper used by fallback-only callers and focused tests.
pub fn help_text(
    auto: bool,
    has_active_question: bool,
    watch: bool,
    prefix: &str,
    lang: Lang,
) -> String {
    render_help_plain(
        &help_view(auto, has_active_question, watch, prefix, lang),
        lang,
    )
}

/// 激活回执文案：基础确认句 +（补推了 N>0 条在途时）追加补推后缀。
pub fn activated_receipt(pending: usize, lang: Lang) -> String {
    let mut s = i18n::tr(lang, "autoChannel.activated").to_string();
    if pending > 0 {
        s.push_str(&i18n::tr(lang, "autoChannel.pending").replace("{n}", &pending.to_string()));
    }
    s
}

/// 反激活提示：活跃槽切到别处时发给**旧**渠道，明确告知切到了哪个渠道（`new_id`，含 "popup"），
/// 后续提问不再走此渠道；发任意消息（自动激活开时切槽即可）即可重新激活。
pub fn deactivated_receipt(new_id: &str, lang: Lang) -> String {
    i18n::tr(lang, "autoChannel.deactivated").replace("{target}", &channel_label(new_id, lang))
}

/// 渠道 id → 展示名（复用「回复来源」文案）。未知 id 原样返回。
pub fn channel_label(id: &str, lang: Lang) -> String {
    let key = match id {
        "popup" => "channel.sourcePopup",
        "telegram" => "channel.sourceTelegram",
        "dingding" => "channel.sourceDingTalk",
        "feishu" => "channel.sourceFeishu",
        "slack" => "channel.sourceSlack",
        other => return other.to_string(),
    };
    i18n::tr(lang, key).to_string()
}

/// 由 agent 注册表快照（`AgentRegistry::snapshot()` 的 Value 数组）组装 `/status` 文本：
/// 仅列「工作中 / 空闲」（已结束不列），工作中在前；空则给「需开启生命周期追踪」提示。
pub fn status_text(snapshot: &Value, lang: Lang) -> String {
    let empty = Vec::new();
    let list = snapshot.as_array().unwrap_or(&empty);

    let mut working: Vec<String> = Vec::new();
    let mut idle: Vec<String> = Vec::new();
    for rec in list {
        let state = rec.get("state").and_then(|v| v.as_str()).unwrap_or("");
        let line = match state {
            "working" => &mut working,
            "idle" => &mut idle,
            _ => continue, // ended / 未知：不列
        };
        line.push(format_line(rec, lang));
    }

    if working.is_empty() && idle.is_empty() {
        return i18n::tr(lang, "autoChannel.statusEmpty").to_string();
    }

    let mut out = String::new();
    if !working.is_empty() {
        out.push_str(i18n::tr(lang, "autoChannel.statusWorking"));
        out.push('\n');
        out.push_str(&working.join("\n"));
    }
    if !idle.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(i18n::tr(lang, "autoChannel.statusIdle"));
        out.push('\n');
        out.push_str(&idle.join("\n"));
    }
    out
}

/// 全局列表单行：`[编号] 类型 — 标题（项目）`。
fn format_line(rec: &Value, lang: Lang) -> String {
    let seq = rec.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
    format!("[{}] {}", seq, kind_title_project(rec, lang))
}

/// `类型 — 标题（项目）`（全局行与详情头部共用；watch 列表也复用）。
pub(crate) fn kind_title_project(rec: &Value, lang: Lang) -> String {
    let kind = rec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let kind_label = crate::agents::AgentKind::parse(kind)
        .map(|k| k.label())
        .unwrap_or(kind);

    let title = rec
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| i18n::tr(lang, "autoChannel.noTitle").to_string());

    let project = rec
        .get("cwd")
        .and_then(|v| v.as_str())
        .and_then(project_name)
        .unwrap_or_else(|| i18n::tr(lang, "autoChannel.noProject").to_string());

    format!("{} — {}（{}）", kind_label, title, project)
}

/// 按 `/status` 稳定编号（seq）在注册表快照中定位记录（`/msg` 寻址复用，spec agent-interject D9）。
pub fn find_by_seq(snapshot: &Value, id: u64) -> Option<&Value> {
    snapshot
        .as_array()?
        .iter()
        .find(|r| r.get("seq").and_then(|v| v.as_u64()) == Some(id))
}

/// `/status <编号>`：单个 agent 的「头部 + 当前活动」。找不到该编号回未找到提示
/// （`prefix` 为命令展示前缀，见 `cmd_prefix`）。
/// 可寻址范围＝快照里的任意记录（工作中 / 空闲 / 已结束皆可）。
pub fn status_detail_text(snapshot: &Value, id: u64, prefix: &str, lang: Lang) -> String {
    let Some(rec) = find_by_seq(snapshot, id) else {
        return i18n::tr(lang, "autoChannel.statusDetailNotFound")
            .replace("{id}", &id.to_string())
            .replace("{p}", prefix);
    };

    // 头部：[编号] 类型 — 标题（项目）· 状态词
    let state = rec.get("state").and_then(|v| v.as_str()).unwrap_or("");
    let state_word = match state {
        "working" => i18n::tr(lang, "autoChannel.stateWorking"),
        "idle" => i18n::tr(lang, "autoChannel.stateIdle"),
        "ended" => i18n::tr(lang, "autoChannel.stateEnded"),
        _ => "",
    };
    let mut out = format!("[{}] {}", id, kind_title_project(rec, lang));
    if !state_word.is_empty() {
        out.push_str(" · ");
        out.push_str(state_word);
    }

    // 当前活动：融合 transcript 尾部与 hook 实时「当前工具」。空行 + 分区标签，明确「agent 输出从这里开始」。
    let parts = activity_parts(rec);

    out.push_str("\n\n");
    if parts.text.is_none() && parts.steps.is_empty() {
        out.push_str(i18n::tr(lang, "autoChannel.statusNoActivity"));
    } else {
        out.push_str(&activity_heading(parts.at, lang));
        if let Some(t) = parts.text {
            out.push('\n');
            out.push_str(&t);
        }
        // 「省略 N 步」标注：文字与展示的 ≤3 步之间还有更早调用时提示。
        if parts.steps_omitted > 0 {
            out.push('\n');
            out.push_str(
                &i18n::tr(lang, "watch.stepsOmitted")
                    .replace("{n}", &parts.steps_omitted.to_string()),
            );
        }
        for step in &parts.steps {
            out.push('\n');
            out.push_str(&render_step(step, lang));
        }
    }
    // TODO 清单摘要（纯文本渠道只给一行；完整清单是飞书 watch 卡折叠面板的能力）。
    if let Some(s) = todo_summary(&parts.todos, lang) {
        out.push('\n');
        out.push_str(&s);
    }
    out
}

/// TODO 清单摘要行：`📋 清单 4/7 · 当前：xxx`（无进行中条目省略「当前」段；空清单 → None）。
/// `/status` 纯文本与飞书 watch 卡折叠面板标题共用。
pub(crate) fn todo_summary(
    todos: &[crate::agents::activity::TodoItem],
    lang: Lang,
) -> Option<String> {
    use crate::agents::activity::TodoState;
    if todos.is_empty() {
        return None;
    }
    let done = todos
        .iter()
        .filter(|t| t.state == TodoState::Completed)
        .count();
    let current = todos
        .iter()
        .find(|t| t.state == TodoState::InProgress)
        .map(|t| t.content.as_str());
    let key = if current.is_some() {
        "watch.todoSummary"
    } else {
        "watch.todoSummaryBare"
    };
    Some(
        i18n::tr(lang, key)
            .replace("{done}", &done.to_string())
            .replace("{total}", &todos.len().to_string())
            .replace("{current}", current.unwrap_or("")),
    )
}

/// 一条注册表快照记录的「当前活动」组成部分（transcript 尾部 × hook 实时工具融合结果）。
/// `/status <编号>` 与 `/watch` 实时卡共用同一份融合逻辑。
pub(crate) struct ActivityParts {
    /// 最后一段助手文字（transcript 侧）。
    pub text: Option<String>,
    /// 最后一段文字之后的足迹时间线（≤3 步，旧→新；实时工具严格更新时并入为进行中的末步）。
    pub steps: Vec<crate::agents::activity::ToolStep>,
    /// 文字之后被挤出时间线的更早调用数（「省略 N 步」标注）。
    pub steps_omitted: usize,
    /// 当前 TODO 清单（TodoWrite / update_plan 重放；agent 未用 todo 功能则为空）。
    pub todos: Vec<crate::agents::activity::TodoItem>,
    /// 实际展示事件的时间（Unix 秒）。
    pub at: Option<u64>,
}

/// 由注册表快照记录计算「当前活动」：读该 session transcript 尾部得到文字 + 足迹时间线，再并入
/// snapshot 注入的实时 `currentTool`（PreToolUse 上报、in-flight 时 transcript 尚未落盘）——
/// 实时工具严格更新时作为「进行中」末步（解决 Cursor「工具跑完才落盘」的滞后）。
pub(crate) fn activity_parts(rec: &Value) -> ActivityParts {
    use crate::agents::activity::ToolStep;
    let kind = rec.get("kind").and_then(|v| v.as_str()).unwrap_or("");
    let sid = rec.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
    let activity = crate::agents::AgentKind::parse(kind)
        .filter(|_| !sid.is_empty())
        .and_then(|k| crate::agents::activity::resolve_activity(k, sid));

    // transcript 侧：助手文字永远取此；足迹与时间用于与实时工具比较。
    let ts_text = activity.as_ref().and_then(|a| a.text.clone());
    let mut steps = activity
        .as_ref()
        .map(|a| a.steps.clone())
        .unwrap_or_default();
    let mut omitted = activity.as_ref().map(|a| a.steps_omitted).unwrap_or(0);
    let todos = activity
        .as_ref()
        .map(|a| a.todos.clone())
        .unwrap_or_default();
    let ts_at = activity.as_ref().and_then(|a| a.at);

    // 实时侧：snapshot 注入的 currentTool。
    let rt = rec.get("currentTool");
    let rt_at = rt.and_then(|t| t.get("at")).and_then(|v| v.as_u64());
    let rt_tool = rt.and_then(build_rt_tool);

    // 融合（内容收敛，spec gui-agent-console 反馈记录 2026-07-25）：旧版按「事件时刻 vs
    // transcript mtime」判先后——mtime 因任何写入前进（渐进写盘 / 落上一条完成调用），会把
    // 仍在跑的实时步秒级挤掉，造成「工具一闪而过」。改为按**内容**收敛：
    // - transcript 尾部已含同款步（同类别+对象）且时间不早于实时事件 → transcript 接管；
    // - 否则实时步驻留为进行中末步，直到被接替 / registry 在 turn-end 清除 / TTL 兜底。
    let tail_has_same = rt_tool
        .as_ref()
        .is_some_and(|td| steps.iter().any(|s| s.tool == *td));
    let use_rt = rt_tool.is_some() && rt_merge_decision(rt_at, ts_at, tail_has_same, now_secs());
    let at = if use_rt {
        if let Some(td) = rt_tool {
            use crate::agents::activity::StepState;
            match steps.last_mut() {
                Some(last) if last.tool == td => last.state = StepState::Running,
                _ => {
                    for s in steps.iter_mut() {
                        if s.state == StepState::Running {
                            s.state = StepState::Done;
                        }
                    }
                    steps.push(ToolStep {
                        tool: td,
                        state: StepState::Running,
                    });
                    if steps.len() > crate::agents::activity::MAX_STEPS {
                        steps.remove(0);
                        omitted += 1;
                    }
                }
            }
        }
        // 展示时刻取两侧较新者（实时步驻留期间 transcript 可能继续前进）。
        match (rt_at, ts_at) {
            (Some(r), Some(t)) => Some(r.max(t)),
            (r, t) => r.or(t),
        }
    } else {
        ts_at
    };
    ActivityParts {
        text: ts_text,
        steps,
        steps_omitted: omitted,
        todos,
        at,
    }
}

/// 实时 currentTool 的呆滞兜底：超过该时长仍未被 transcript 接替 / turn-end 清除（漏 hook /
/// 会话异常中断）→ 不再展示为进行中。
const RT_TOOL_TTL_SECS: u64 = 600;

/// 实时工具取舍（内容收敛，抽出便于单测）：
/// - 事件太旧（TTL）→ 弃用；
/// - transcript 尾部已含同款步且时间不早于实时事件 → transcript 接管（弃用）；
/// - 其余情况驻留为进行中末步（mtime 因无关写入前进不构成接替——旧版据此秒级丢弃，
///   造成「工具一闪而过」，spec gui-agent-console 反馈记录 2026-07-25）。
fn rt_merge_decision(
    rt_at: Option<u64>,
    ts_at: Option<u64>,
    tail_has_same: bool,
    now: u64,
) -> bool {
    let fresh = rt_at.is_some_and(|t| now.saturating_sub(t) <= RT_TOOL_TTL_SECS);
    if !fresh {
        return false;
    }
    !tail_has_same || realtime_newer(rt_at, ts_at)
}

/// 由 snapshot 的 `currentTool`（`{name, object, at}`）构造工具展示。类别标签按原始工具名复得，
/// 对象用已归一化的存量值。无有效工具名 / TODO 类工具（不入时间线）→ None。
fn build_rt_tool(rt: &Value) -> Option<crate::agents::activity::ToolDisplay> {
    let name = rt.get("name").and_then(|v| v.as_str()).unwrap_or("").trim();
    if name.is_empty() || crate::agents::activity::is_todo_tool(name) {
        return None;
    }
    let object = rt
        .get("object")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let label = crate::agents::activity::classify_tool(name, None).label;
    Some(crate::agents::activity::ToolDisplay { label, object })
}

/// 实时工具是否比 transcript 尾部工具**更新**（严格）：`rt` 存在且时间严格晚于 `ts`，或 transcript
/// 无时间（尚未落盘）→ true。等于/更旧 → false（兜底弃用实时工具，防丢 PostToolUse 残留）。
fn realtime_newer(rt_at: Option<u64>, ts_at: Option<u64>) -> bool {
    match (rt_at, ts_at) {
        (Some(r), Some(t)) => r > t,
        (Some(_), None) => true,
        _ => false,
    }
}

/// 「最近动态」分区标签，带相对时间：zh `最近动态（3 秒前）：` / en `Latest activity (3s ago):`。
/// `at` 缺失时省略括号。
fn activity_heading(at: Option<u64>, lang: Lang) -> String {
    let heading = i18n::tr(lang, "autoChannel.activityHeading");
    let rel = at.map(|ts| rel_time(now_secs(), ts, lang));
    match (lang, rel) {
        (Lang::Zh, Some(r)) => format!("{heading}（{r}）："),
        (Lang::Zh, None) => format!("{heading}："),
        (_, Some(r)) => format!("{heading} ({r}):"),
        (_, None) => format!("{heading}:"),
    }
}

/// 相对时间标注（供「最近动态」用）。<5s → 刚刚；否则秒 / 分钟 / 小时 / 天前。
fn rel_time(now: u64, ts: u64, lang: Lang) -> String {
    let d = now.saturating_sub(ts);
    match lang {
        Lang::Zh => {
            if d < 5 {
                "刚刚".to_string()
            } else if d < 60 {
                format!("{d} 秒前")
            } else if d < 3600 {
                format!("{} 分钟前", d / 60)
            } else if d < 86400 {
                format!("{} 小时前", d / 3600)
            } else {
                format!("{} 天前", d / 86400)
            }
        }
        Lang::En => {
            if d < 5 {
                "just now".to_string()
            } else if d < 60 {
                format!("{d}s ago")
            } else if d < 3600 {
                format!("{}m ago", d / 60)
            } else if d < 86400 {
                format!("{}h ago", d / 3600)
            } else {
                format!("{}d ago", d / 86400)
            }
        }
    }
}

/// 足迹时间线一步的类别词与对象（用户定案：不再用类别 emoji，只保留状态圆点；类别词由
/// 渲染侧决定加粗与否）。返回 `(类别词/原始工具名, 对象)`。
pub(crate) fn step_label_object(
    step: &crate::agents::activity::ToolStep,
    lang: Lang,
) -> (String, Option<String>) {
    use crate::agents::activity::ToolLabel;
    let label = match &step.tool.label {
        ToolLabel::Run => i18n::tr(lang, "autoChannel.activityRun").to_string(),
        ToolLabel::Read => i18n::tr(lang, "autoChannel.activityRead").to_string(),
        ToolLabel::Write => i18n::tr(lang, "autoChannel.activityWrite").to_string(),
        ToolLabel::Other(name) => name.clone(),
    };
    (label, step.tool.object.clone())
}

/// `/status` 用纯文本步行：状态圆点（进行中 🟢 / 已完成 ⚪ / 失败 🔴）+ `类别词: 对象`。
/// （飞书 watch 卡走 `watch::` 侧的彩色 `<font>` 圆点 + 粗体/斜体渲染，不经此函数。）
pub(crate) fn render_step(step: &crate::agents::activity::ToolStep, lang: Lang) -> String {
    use crate::agents::activity::StepState;
    let dot = match step.state {
        StepState::Running => "🟢",
        StepState::Done => "⚪",
        StepState::Failed => "🔴",
    };
    let (label, object) = step_label_object(step, lang);
    match object {
        Some(o) => format!("{} {}: {}", dot, label, o),
        None => format!("{} {}", dot, label),
    }
}

/// 取工作目录的末段作为项目名（空 → None）。
pub(crate) fn project_name(cwd: &str) -> Option<String> {
    let trimmed = cwd.trim_end_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    std::path::Path::new(trimmed)
        .file_name()
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn todo_summary_counts_and_current() {
        use crate::agents::activity::{TodoItem, TodoState};
        let item = |content: &str, state: TodoState| TodoItem {
            content: content.into(),
            state,
        };
        // 空清单 → None。
        assert!(todo_summary(&[], Lang::Zh).is_none());
        // 完成计数 + 当前进行中条目。
        let todos = vec![
            item("改 registry", TodoState::Completed),
            item("跑单测", TodoState::InProgress),
            item("更新文档", TodoState::Pending),
        ];
        assert_eq!(
            todo_summary(&todos, Lang::Zh).as_deref(),
            Some("📋 TODO 1/3 · 当前：跑单测")
        );
        // 无进行中条目 → 省略「当前」段。
        let done = vec![
            item("改 registry", TodoState::Completed),
            item("跑单测", TodoState::Completed),
        ];
        assert_eq!(
            todo_summary(&done, Lang::Zh).as_deref(),
            Some("📋 TODO 2/2")
        );
    }

    /// 实时工具取舍（内容收敛，spec gui-agent-console 反馈记录 2026-07-25）。
    #[test]
    fn rt_merge_decision_is_content_based() {
        let now = 1_000_000u64;
        // 旧版闪烁场景：transcript mtime 已追平/超过实时事件，但尾部**没有**同款步 → 驻留。
        assert!(rt_merge_decision(Some(now - 3), Some(now), false, now));
        // 同秒也不判负（旧版 (Some(r), Some(t)) 要求 r > t）。
        assert!(rt_merge_decision(Some(now), Some(now), false, now));
        // 尾部已含同款步且 transcript 不早于实时事件 → transcript 接管。
        assert!(!rt_merge_decision(Some(now - 3), Some(now), true, now));
        // 尾部含同款步但实时事件更新（同一工具再次被调）→ 仍并入为进行中。
        assert!(rt_merge_decision(Some(now), Some(now - 3), true, now));
        // TTL 呆滞兜底。
        assert!(!rt_merge_decision(
            Some(now - RT_TOOL_TTL_SECS - 1),
            None,
            false,
            now
        ));
        // 无时刻的实时事件不可信 → 弃用。
        assert!(!rt_merge_decision(None, Some(now), false, now));
    }

    #[test]
    fn classify_commands_and_synonyms() {
        assert_eq!(
            classify("/new"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(
            classify("!new"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(
            classify("/新任务"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(
            classify("/new do work"),
            Parsed::Command(Command::New { has_args: true })
        );
        assert_eq!(classify("/here"), Parsed::Command(Command::Here));
        assert_eq!(classify(" /这里 "), Parsed::Command(Command::Here));
        assert_eq!(classify("/status"), Parsed::Command(Command::Status(None)));
        assert_eq!(classify("/状态"), Parsed::Command(Command::Status(None)));
        assert_eq!(classify("/help"), Parsed::Command(Command::Help));
        assert_eq!(classify("/帮助"), Parsed::Command(Command::Help));
        assert_eq!(classify("/?"), Parsed::Command(Command::Help));
        assert_eq!(classify("/？"), Parsed::Command(Command::Help));
    }

    #[test]
    fn command_phrase_keys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for (key, _) in COMMAND_PHRASES {
            assert!(seen.insert(*key), "duplicate command phrase key: {key}");
        }
    }

    #[test]
    fn classify_bare_phrases_map_to_no_arg_commands() {
        assert_eq!(
            classify("新建会话"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(
            classify("新建"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(
            classify("新建 会话！"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(
            classify("new"),
            Parsed::Command(Command::New { has_args: false })
        );
        assert_eq!(classify("状态"), Parsed::Command(Command::Status(None)));
        assert_eq!(classify("status"), Parsed::Command(Command::Status(None)));
        assert_eq!(classify("插话"), Parsed::Command(Command::Msg(None, None)));
        assert_eq!(
            classify("发消息"),
            Parsed::Command(Command::Msg(None, None))
        );
        assert_eq!(classify("待办"), Parsed::Command(Command::Todo(None, None)));
        assert_eq!(classify("todo"), Parsed::Command(Command::Todo(None, None)));
        assert_eq!(classify("删待办"), Parsed::Command(Command::TodoRm(None)));
        assert_eq!(
            classify("自动待办"),
            Parsed::Command(Command::TodoAuto(None, None))
        );
        assert_eq!(classify("关注"), Parsed::Command(Command::Watch(None)));
        assert_eq!(
            classify("取消关注"),
            Parsed::Command(Command::Unwatch(WatchSel::Auto))
        );
        assert_eq!(classify("这里"), Parsed::Command(Command::Here));
        assert_eq!(classify("帮助"), Parsed::Command(Command::Help));
        assert_eq!(classify("diff"), Parsed::Command(Command::Diff(None)));
        assert_eq!(classify("暂存"), Parsed::Command(Command::Stage(None)));
        assert_eq!(
            classify("导出会话"),
            Parsed::Command(Command::Transcript(None))
        );
    }

    #[test]
    fn classify_phrase_does_not_match_partial_or_prefixed() {
        // Longer free text stays Text.
        assert_eq!(classify("请帮我新建会话"), Parsed::Text);
        assert_eq!(classify("新建会话吧"), Parsed::Text);
        // Slash path still wins and can carry args.
        assert_eq!(
            classify("/new do work"),
            Parsed::Command(Command::New { has_args: true })
        );
        // Unknown bang stays free text.
        assert_eq!(classify("!important"), Parsed::Text);
    }

    #[test]
    fn classify_status_with_id() {
        assert_eq!(
            classify("/status 3"),
            Parsed::Command(Command::Status(Some(3)))
        );
        assert_eq!(
            classify("/状态 12"),
            Parsed::Command(Command::Status(Some(12)))
        );
        // 非数字参数 → 全局。
        assert_eq!(
            classify("/status abc"),
            Parsed::Command(Command::Status(None))
        );
    }

    #[test]
    fn classify_diff_stage_transcript() {
        assert_eq!(classify("/diff"), Parsed::Command(Command::Diff(None)));
        assert_eq!(classify("/diff 3"), Parsed::Command(Command::Diff(Some(3))));
        assert_eq!(
            classify("!stage 2"),
            Parsed::Command(Command::Stage(Some(2)))
        );
        assert_eq!(
            classify("/transcript"),
            Parsed::Command(Command::Transcript(None))
        );
        assert_eq!(
            classify("/transcript 9"),
            Parsed::Command(Command::Transcript(Some(9)))
        );
        // 非数字 → 无参。
        assert_eq!(classify("/diff abc"), Parsed::Command(Command::Diff(None)));
    }

    #[test]
    fn classify_is_case_insensitive_and_takes_first_token() {
        assert_eq!(classify("/HELP"), Parsed::Command(Command::Help));
        // "now" 非数字 → 全局。
        assert_eq!(
            classify("/Status now"),
            Parsed::Command(Command::Status(None))
        );
    }

    #[test]
    fn classify_unknown_command_vs_plain_text() {
        assert_eq!(classify("/foobar"), Parsed::UnknownCommand);
        assert_eq!(classify("/"), Parsed::UnknownCommand);
        assert_eq!(classify("hello"), Parsed::Text);
        assert_eq!(classify("  not a command /here"), Parsed::Text);
        assert_eq!(classify(""), Parsed::Text);
    }

    #[test]
    fn classify_bang_prefix() {
        // `!` 备用前缀（Slack 拦截 `/`）：已知命令等价于斜线版。
        assert_eq!(classify("!status"), Parsed::Command(Command::Status(None)));
        assert_eq!(
            classify("!watch 3"),
            Parsed::Command(Command::Watch(Some(3)))
        );
        assert_eq!(
            classify("!unwatch all"),
            Parsed::Command(Command::Unwatch(WatchSel::All))
        );
        assert_eq!(classify("!here"), Parsed::Command(Command::Here));
        assert_eq!(classify(" !HELP "), Parsed::Command(Command::Help));
        // 未知 `!xxx` 是普通文本（不能劫持感叹号开头的自由回答），区别于 `/xxx`。
        assert_eq!(classify("!important note"), Parsed::Text);
        assert_eq!(classify("!"), Parsed::Text);
        assert_eq!(classify("!!"), Parsed::Text);
    }

    #[test]
    fn help_text_gates_on_auto_activation() {
        let switch = i18n::tr(Lang::En, "autoChannel.helpSwitchHint");
        // auto on → lists /here + switch hint.
        let on = help_text(true, false, false, "/", Lang::En);
        assert!(on.contains("/here"));
        assert!(on.contains(switch));
        // auto off → neither /here nor switch hint.
        let off = help_text(false, false, false, "/", Lang::En);
        assert!(!off.contains("/here"));
        assert!(!off.contains(switch));
    }

    #[test]
    fn help_text_gates_on_active_question() {
        let answering = i18n::tr(Lang::En, "autoChannel.helpAnswering");
        let none = i18n::tr(Lang::En, "autoChannel.helpNoQuestion");
        let with_q = help_text(false, true, false, "/", Lang::En);
        assert!(with_q.contains(answering));
        assert!(!with_q.contains(none));
        let without_q = help_text(false, false, false, "/", Lang::En);
        assert!(without_q.contains(none));
        assert!(!without_q.contains(answering));
    }

    #[test]
    fn help_text_gates_on_watch_support() {
        let supported = help_text(false, false, true, "/", Lang::En);
        assert!(supported.contains("/watch [n]"));
        assert!(supported.contains("/unwatch [n|all]"));
        let unsupported = help_text(false, false, false, "/", Lang::En);
        assert!(!unsupported.contains("/watch [n]"));
        assert!(!unsupported.contains("/unwatch [n|all]"));
    }

    #[test]
    fn help_text_uses_channel_prefix() {
        // Slack 展示 `!` 前缀（客户端拦截 `/`）；其余渠道 `/`。
        assert_eq!(cmd_prefix("slack"), "!");
        assert_eq!(cmd_prefix("feishu"), "/");
        let slack = help_text(true, false, true, cmd_prefix("slack"), Lang::En);
        assert!(slack.contains("!status"));
        assert!(slack.contains("!watch"));
        assert!(slack.contains("!help"));
        assert!(!slack.contains("/status"));
        assert!(!slack.contains("{p}"));
        let feishu = help_text(true, false, true, cmd_prefix("feishu"), Lang::En);
        assert!(feishu.contains("/status"));
        assert!(!feishu.contains("{p}"));
    }

    #[test]
    fn classify_watch_and_unwatch() {
        assert_eq!(classify("/watch"), Parsed::Command(Command::Watch(None)));
        assert_eq!(
            classify("/关注 3"),
            Parsed::Command(Command::Watch(Some(3)))
        );
        assert_eq!(
            classify("/watch 12"),
            Parsed::Command(Command::Watch(Some(12)))
        );
        // 非数字参数 → 列表（同 /status 的宽松处理）。
        assert_eq!(
            classify("/watch abc"),
            Parsed::Command(Command::Watch(None))
        );
        assert_eq!(
            classify("/unwatch"),
            Parsed::Command(Command::Unwatch(WatchSel::Auto))
        );
        assert_eq!(
            classify("/unwatch 5"),
            Parsed::Command(Command::Unwatch(WatchSel::One(5)))
        );
        assert_eq!(
            classify("/unwatch all"),
            Parsed::Command(Command::Unwatch(WatchSel::All))
        );
        assert_eq!(
            classify("/取消关注 全部"),
            Parsed::Command(Command::Unwatch(WatchSel::All))
        );
        assert_eq!(
            classify("/UNWATCH ALL"),
            Parsed::Command(Command::Unwatch(WatchSel::All))
        );
    }

    #[test]
    fn classify_yolo_variants() {
        assert_eq!(
            classify("/yolo"),
            Parsed::Command(Command::Yolo(YoloSel::List))
        );
        assert_eq!(
            classify("!yolo"),
            Parsed::Command(Command::Yolo(YoloSel::List))
        );
        assert_eq!(
            classify("/yolo off"),
            Parsed::Command(Command::Yolo(YoloSel::Off(None)))
        );
        assert_eq!(
            classify("/yolo OFF 3"),
            Parsed::Command(Command::Yolo(YoloSel::Off(Some(3))))
        );
        assert_eq!(
            classify("/yolo 关闭 2"),
            Parsed::Command(Command::Yolo(YoloSel::Off(Some(2))))
        );
        // 未知子命令 → 列表（宽松处理，同 /watch 非数字参数）。
        assert_eq!(
            classify("/yolo abc"),
            Parsed::Command(Command::Yolo(YoloSel::List))
        );
        // 无前缀整句短语（spec im-command-phrases）：列表卡自带关闭按钮，一律指向列表。
        for phrase in [
            "yolo",
            "YOLO 模式",
            "关闭 yolo",
            "yolo off",
            "turn off yolo",
        ] {
            assert_eq!(
                classify(phrase),
                Parsed::Command(Command::Yolo(YoloSel::List)),
                "phrase: {phrase}"
            );
        }
        // 含额外内容的整句不是短语命令（避免吞掉作答文本）。
        assert_eq!(classify("我觉得 yolo 就行"), Parsed::Text);
    }

    #[test]
    fn classify_msg_and_msg_clear() {
        // `/msg <编号> <内容>`：内容为编号后的原文，保留内部换行 / 空白。
        assert_eq!(
            classify("/msg 3 停一下，先看测试"),
            Parsed::Command(Command::Msg(Some(3), Some("停一下，先看测试".to_string())))
        );
        assert_eq!(
            classify("/msg 3 第一行\n  第二行"),
            Parsed::Command(Command::Msg(Some(3), Some("第一行\n  第二行".to_string())))
        );
        // 有编号无内容 → 回显。
        assert_eq!(
            classify("/msg 3"),
            Parsed::Command(Command::Msg(Some(3), None))
        );
        // 无编号无内容 → (None, None)（增强用法提示）。
        assert_eq!(classify("/msg"), Parsed::Command(Command::Msg(None, None)));
        // 无编号有内容（首 token 非数字）→ 整段作内容，交自动选择流程。
        assert_eq!(
            classify("/msg hello"),
            Parsed::Command(Command::Msg(None, Some("hello".to_string())))
        );
        assert_eq!(
            classify("/msg 停一下，先看测试"),
            Parsed::Command(Command::Msg(None, Some("停一下，先看测试".to_string())))
        );
        // 中文别名 + `!` 备用前缀（Slack）。
        assert_eq!(
            classify("/插话 2 换个方案"),
            Parsed::Command(Command::Msg(Some(2), Some("换个方案".to_string())))
        );
        assert_eq!(
            classify("!msg 1 hi"),
            Parsed::Command(Command::Msg(Some(1), Some("hi".to_string())))
        );
        // msg-clear / 撤回。
        assert_eq!(
            classify("/msg-clear 3"),
            Parsed::Command(Command::MsgClear(Some(3)))
        );
        assert_eq!(
            classify("/撤回 3"),
            Parsed::Command(Command::MsgClear(Some(3)))
        );
        assert_eq!(
            classify("/msg-clear"),
            Parsed::Command(Command::MsgClear(None))
        );
        assert_eq!(
            classify("!MSG-CLEAR 7"),
            Parsed::Command(Command::MsgClear(Some(7)))
        );
    }

    #[test]
    fn classify_fork_accepts_only_an_optional_session_number() {
        assert_eq!(
            classify("/fork"),
            Parsed::Command(Command::Fork {
                sel: None,
                has_invalid_args: false,
            })
        );
        assert_eq!(
            classify("!fork 12"),
            Parsed::Command(Command::Fork {
                sel: Some(12),
                has_invalid_args: false,
            })
        );
        assert_eq!(
            classify("/分叉 3 continue here"),
            Parsed::Command(Command::Fork {
                sel: Some(3),
                has_invalid_args: true,
            })
        );
        assert_eq!(
            classify("/fork current"),
            Parsed::Command(Command::Fork {
                sel: None,
                has_invalid_args: true,
            })
        );
    }

    #[test]
    fn classify_todo_and_todo_rm() {
        // `/todo`（无参）→ 选项目管理；非数字文本 → 选项目新增；数字入口保持向后兼容。
        assert_eq!(
            classify("/todo"),
            Parsed::Command(Command::Todo(None, None))
        );
        assert_eq!(
            classify("/todo 3"),
            Parsed::Command(Command::Todo(Some(3), None))
        );
        assert_eq!(
            classify("/todo 3 修复登录\n再跑测试"),
            Parsed::Command(Command::Todo(
                Some(3),
                Some("修复登录\n再跑测试".to_string())
            ))
        );
        // 无编号有内容（首 token 非数字）→ 由调用方回用法提示。
        assert_eq!(
            classify("/todo fix login"),
            Parsed::Command(Command::Todo(None, Some("fix login".to_string())))
        );
        // 中文同义词 + `!` 前缀。
        assert_eq!(
            classify("/待办 2 写文档"),
            Parsed::Command(Command::Todo(Some(2), Some("写文档".to_string())))
        );
        assert_eq!(
            classify("!todo"),
            Parsed::Command(Command::Todo(None, None))
        );
        // `/todo-rm`。
        assert_eq!(classify("/todo-rm"), Parsed::Command(Command::TodoRm(None)));
        assert_eq!(
            classify("/todo-rm 5"),
            Parsed::Command(Command::TodoRm(Some(5)))
        );
        assert_eq!(classify("/删待办"), Parsed::Command(Command::TodoRm(None)));
        // 非数字参数 → 无参（同 /diff 的宽松处理）。
        assert_eq!(
            classify("/todo-rm abc"),
            Parsed::Command(Command::TodoRm(None))
        );
    }

    #[test]
    fn classify_todo_auto() {
        // 语法镜像 /todo：无编号 → 选项目；数字入口保持向后兼容。
        assert_eq!(
            classify("/todo-auto"),
            Parsed::Command(Command::TodoAuto(None, None))
        );
        assert_eq!(
            classify("/todo-auto 3"),
            Parsed::Command(Command::TodoAuto(Some(3), None))
        );
        assert_eq!(
            classify("/todo-auto 3 每晚跑回归"),
            Parsed::Command(Command::TodoAuto(Some(3), Some("每晚跑回归".to_string())))
        );
        // 首 token 非数字 → 无编号有内容（调用方回用法提示）。
        assert_eq!(
            classify("/todo-auto run tests"),
            Parsed::Command(Command::TodoAuto(None, Some("run tests".to_string())))
        );
        // 中文同义词。
        assert_eq!(
            classify("/自动待办 2"),
            Parsed::Command(Command::TodoAuto(Some(2), None))
        );
    }

    #[test]
    fn help_text_lists_todo_commands() {
        for lang in [Lang::En, Lang::Zh] {
            let t = help_text(true, false, true, "/", lang);
            assert!(t.contains("todo"), "{t}");
            assert!(t.contains("todo-rm"), "{t}");
            assert!(t.contains("todo-auto"), "{t}");
            let off = help_text(false, false, false, "/", lang);
            assert!(off.contains("todo") && off.contains("todo-rm") && off.contains("todo-auto"));
        }
    }

    #[test]
    fn help_text_lists_diff_stage_transcript() {
        // /diff · /stage · /transcript 与 /status 同门控：help 始终列出（spec D23）。
        for lang in [Lang::En, Lang::Zh] {
            let t = help_text(true, false, true, "/", lang);
            assert!(t.contains("diff"), "{t}");
            assert!(t.contains("stage"), "{t}");
            assert!(t.contains("transcript"), "{t}");
            let off = help_text(false, false, false, "/", lang);
            assert!(off.contains("diff") && off.contains("stage") && off.contains("transcript"));
        }
    }

    #[test]
    fn help_text_lists_yolo() {
        for lang in [Lang::En, Lang::Zh] {
            assert!(help_text(true, false, true, "/", lang).contains("/yolo"));
            assert!(help_text(false, false, false, "/", lang).contains("/yolo"));
        }
    }

    #[test]
    fn help_text_always_lists_msg() {
        // /msg 与 /status 同门控：任何开关组合都在 help 中列出。
        assert!(help_text(false, false, false, "/", Lang::En).contains("/msg [n] [text]"));
        assert!(help_text(true, true, true, "/", Lang::En).contains("/msg [n] [text]"));
    }

    #[test]
    fn help_view_has_grouped_complete_command_set() {
        let view = help_view(true, false, true, "/", Lang::Zh);
        assert_eq!(view.sections.len(), 4);
        assert_eq!(view.sections[0].title, "Agent 管理");
        assert_eq!(view.sections[1].title, "代码与记录");
        assert_eq!(view.sections[2].title, "项目待办");
        assert_eq!(view.sections[3].title, "渠道与帮助");

        let syntaxes: Vec<&str> = view
            .sections
            .iter()
            .flat_map(|section| section.commands.iter())
            .map(|command| command.syntax.as_str())
            .collect();
        assert_eq!(syntaxes.len(), 16);
        assert_eq!(
            syntaxes,
            vec![
                "/status [编号]",
                "/new",
                "/fork [编号]",
                "/watch [编号]",
                "/unwatch [编号|all]",
                "/msg [编号] [内容]",
                "/msg-clear <编号>",
                "/yolo [off [编号]]",
                "/diff [编号]",
                "/stage [编号]",
                "/transcript [编号]",
                "/todo [内容]",
                "/todo-rm",
                "/todo-auto [内容]",
                "/here",
                "/help",
            ]
        );
        assert_eq!(
            view.sections[0].commands[6].phrase, None,
            "/msg-clear must not advertise an unsupported bare phrase"
        );
    }

    #[test]
    fn help_view_phrases_are_real_bare_commands() {
        for lang in [Lang::En, Lang::Zh] {
            let view = help_view(true, false, true, "/", lang);
            for command in view
                .sections
                .iter()
                .flat_map(|section| section.commands.iter())
            {
                let Some(phrase) = &command.phrase else {
                    assert!(command.syntax.starts_with("/msg-clear"));
                    continue;
                };
                assert!(
                    matches!(classify(phrase), Parsed::Command(_)),
                    "advertised phrase must parse as a command: {phrase}"
                );
            }
        }
    }

    #[test]
    fn plain_help_is_grouped_bulleted_and_markup_free() {
        let text = help_text(true, false, true, "/", Lang::Zh);
        assert!(text.contains("\n\nAgent 管理\n• /status [编号]"));
        assert!(text.contains("· 直接说「状态」"));
        assert!(text.contains("\n\n代码与记录\n• /diff [编号]"));
        assert!(!text.contains('`'));
        assert!(!text.contains("<font"));
    }

    #[test]
    fn find_by_seq_locates_record() {
        let snap = serde_json::json!([
            { "seq": 1, "sessionId": "s1", "kind": "claude", "state": "working" },
            { "seq": 2, "sessionId": "s2", "kind": "grok", "state": "idle" },
        ]);
        assert_eq!(
            find_by_seq(&snap, 2)
                .and_then(|r| r.get("sessionId"))
                .and_then(|v| v.as_str()),
            Some("s2")
        );
        assert!(find_by_seq(&snap, 9).is_none());
        assert!(find_by_seq(&serde_json::json!({}), 1).is_none());
    }

    #[test]
    fn answer_ack_distinguishes_kind_and_mode() {
        // Card vs Fallback differ; kinds differ.
        let img_card = answer_ack_text(AckKind::Image, AckMode::Card, Lang::En);
        let img_fb = answer_ack_text(AckKind::Image, AckMode::Fallback, Lang::En);
        assert_ne!(img_card, img_fb);
        let file_card = answer_ack_text(AckKind::File, AckMode::Card, Lang::En);
        assert_ne!(img_card, file_card);
    }

    #[test]
    fn detect_ack_inserts_field_without_id() {
        let field = i18n::tr(Lang::En, "autoChannel.detectFieldUserId");
        let out = detect_ack_text(field, Lang::En);
        assert!(out.contains(field));
        assert!(!out.contains("{field}"));
    }

    #[test]
    fn status_text_prefixes_seq() {
        let snap = serde_json::json!([
            {"seq":2,"kind":"cursor","sessionId":"s","state":"working","title":"t","cwd":"/a/proj"}
        ]);
        let out = status_text(&snap, Lang::En);
        assert!(out.contains("[2] "));
    }

    #[test]
    fn rel_time_buckets_and_heading() {
        let base = 1_000_000u64;
        assert_eq!(rel_time(base + 2, base, Lang::Zh), "刚刚");
        assert_eq!(rel_time(base + 30, base, Lang::Zh), "30 秒前");
        assert_eq!(rel_time(base + 120, base, Lang::Zh), "2 分钟前");
        assert_eq!(rel_time(base + 7200, base, Lang::Zh), "2 小时前");
        assert_eq!(rel_time(base + 30, base, Lang::En), "30s ago");
        // 标签带相对时间括号；缺时间省略括号。
        let h = activity_heading(Some(now_secs()), Lang::Zh);
        assert!(h.starts_with("最近动态（"));
        assert!(h.ends_with("）："));
        assert_eq!(activity_heading(None, Lang::Zh), "最近动态：");
        assert_eq!(activity_heading(None, Lang::En), "Latest activity:");
    }

    #[test]
    fn realtime_tool_fusion_decision() {
        // 实时更新（严格）→ 用实时。
        assert!(realtime_newer(Some(100), Some(90)));
        // transcript 无时间（尚未落盘）→ 用实时。
        assert!(realtime_newer(Some(100), None));
        // 相等 / 更旧 → 弃用实时（用 transcript）。
        assert!(!realtime_newer(Some(100), Some(100)));
        assert!(!realtime_newer(Some(80), Some(100)));
        assert!(!realtime_newer(None, Some(100)));
        // build_rt_tool：有名出工具（Shell→运行命令类别），无名 None。
        let td = build_rt_tool(&serde_json::json!({"name":"Shell","object":"cargo test","at":1}))
            .unwrap();
        assert_eq!(td.label, crate::agents::activity::ToolLabel::Run);
        assert_eq!(td.object.as_deref(), Some("cargo test"));
        assert!(build_rt_tool(&serde_json::json!({"name":"","at":1})).is_none());
    }

    #[test]
    fn status_detail_not_found_and_no_activity() {
        let snap = serde_json::json!([
            {"seq":1,"kind":"cursor","sessionId":"no-such-session-xyz","state":"working","title":"做点事","cwd":"/tmp/proj"}
        ]);
        // 未找到编号 → 含 id 提示（命令按渠道前缀渲染）。
        let nf = status_detail_text(&snap, 9, "!", Lang::En);
        assert!(nf.contains('9'));
        assert!(nf.contains("!status"));
        // 命中：头部含编号与标题；无会话文件 → 无活动提示。
        let d = status_detail_text(&snap, 1, "/", Lang::En);
        assert!(d.contains("[1]"));
        assert!(d.contains("做点事"));
        assert!(d.contains(i18n::tr(Lang::En, "autoChannel.statusNoActivity")));
    }
}
