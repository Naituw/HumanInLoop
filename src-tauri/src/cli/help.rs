//! 帮助与版本文案（按界面语言本地化，源语言英文）。
//!
//! `--agent-help`（面向 AI 提问）与 `--scripting-help`（面向脚本/自动化）共用若干片段
//! （`ask_arg_lines` / `result_field_lines` / `script_flag_lines` / `exit_code_lines`），
//! 各自取所需拼装，避免重复维护。结果字段标记取自 `output::MARKER_*` 常量，确保文档与实际输出一致。

use super::output;
use crate::i18n::Lang;
use std::path::Path;

/// 用于帮助文案的程序名，取自 argv[0] 的 basename。
/// 为空或异常时回退到 "AskHuman"。这样任何包装器/软链/改名调用
/// 都会显示对应的名字，无需调用方额外配合。
pub fn program_name() -> String {
    program_name_from(std::env::args().next().as_deref())
}

fn program_name_from(arg0: Option<&str>) -> String {
    arg0.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            Path::new(s)
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| s.to_string())
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "AskHuman".to_string())
}

/// `--help`：完整功能，按「提问 / 管理 / 帮助」三块组织（spec §4.1）。
pub fn help_text(lang: Lang) -> String {
    let prog = program_name();
    match lang {
        Lang::En => [
            format!("{prog} - Human-in-the-loop interaction tool"),
            String::new(),
            "Usage:".to_string(),
            format!("  {prog} <message> [options]      Ask a human and collect their response"),
            String::new(),
            "Asking (agents: see --agent-help · scripts/automation: see --scripting-help):".to_string(),
            "  -q, --question <text>   Ask a question; repeatable".to_string(),
            "  -o, --option <text>     Add a predefined answer option after a question".to_string(),
            "  -o!, --option! <text>   Same as -o, marks it as your recommended answer".to_string(),
            "  -f, --file <path>       Attach a file/image to the message; repeatable".to_string(),
            "  --stdin                 Read the message from stdin via a quoted heredoc (last on the command line)".to_string(),
            "  --single                Single choice (default: multiple choice)".to_string(),
            "  --select-only           Choice only: forbid free text/attachments (each question must have options)".to_string(),
            "  --output <text|json>    Output format (default: text)".to_string(),
            "  --whats-next            Ask \"what should we do next?\" with optional suggested tasks and project todos (see --agent-help)".to_string(),
            String::new(),
            "Management:".to_string(),
            "  --settings              Open the settings window".to_string(),
            "  --history [--all]       Open the reply history window (current project; --all for every project)".to_string(),
            "  --show-last [N]         Print recent completed AskHuman exchange(s) for this Agent session (N=1–10, default 1)".to_string(),
            "  --todos                 Open the project todos window (preselects current project)".to_string(),
            "  daemon <sub>            Manage the background daemon: status/stop/restart/start/logs (stop/restart drain active requests; add --force to terminate now)".to_string(),
            "  update prepare          Windows: drain background processes before a manual update".to_string(),
            "  mcp                     Run as an MCP server over STDIO, exposing ask, whats_next, show_last, todo_add, todo_list, and todo_update".to_string(),
            "  todo <sub>              Project todos: add [--auto] [-f path] <text> / list / attach / detach / rm / clear".to_string(),
            "  channel <sub>           Configure IM channels without a GUI (list/set/enable/disable/test/detect; see 'channel help')".to_string(),
            "  agents <sub>            Agent status & integrations (monitor/show/install/uninstall/update; see 'agents help')".to_string(),
            "  config <sub>            Generic config key/value fallback (show/get/set/unset/path; see 'config help')".to_string(),
            "  doctor [--json]         One-screen health check (daemon, channels, integrations)".to_string(),
            String::new(),
            "Help:".to_string(),
            "  --agent-help            Concise usage tuned for AI agents (asking)".to_string(),
            "  --scripting-help        Usage for scripts/automation (choice-only, single, JSON output)".to_string(),
            "  --help, -h              Show this help".to_string(),
            "  --version, -v           Show version".to_string(),
        ]
        .join("\n"),
        Lang::Zh => [
            format!("{prog} - Human-In-The-Loop 交互工具"),
            String::new(),
            "用法:".to_string(),
            format!("  {prog} <message> [选项]         向人类提问并收集回应"),
            String::new(),
            "提问（AI 见 --agent-help · 脚本/自动化见 --scripting-help）:".to_string(),
            "  -q, --question <text>   提出问题，可多次出现".to_string(),
            "  -o, --option <text>     跟随问题之后，添加预定义回答选项".to_string(),
            "  -o!, --option! <text>   同 -o，并标记为你的推荐答案".to_string(),
            "  -f, --file <path>       为消息附带文件/图片，可多次出现".to_string(),
            "  --stdin                 用带引号 heredoc 从标准输入读取消息（写在命令最后）".to_string(),
            "  --single                单选（默认多选）".to_string(),
            "  --select-only           严格选择：禁用自由文本/附件（每题必须有选项）".to_string(),
            "  --output <text|json>    输出格式（默认 text）".to_string(),
            "  --whats-next            提问「接下来做什么？」，可附建议任务，本项目待办也作为选项（见 --agent-help）".to_string(),
            String::new(),
            "管理:".to_string(),
            "  --settings              启动设置界面".to_string(),
            "  --history [--all]       启动回复历史窗口（默认当前项目；--all 查看全部项目）".to_string(),
            "  --show-last [N]         输出当前 Agent 会话最近 N 条已完成 AskHuman 问答（N=1–10，默认 1）".to_string(),
            "  --todos                 启动项目待办窗口（预选当前项目）".to_string(),
            "  daemon <子命令>          管理后台 daemon：status/stop/restart/start/logs（stop/restart 默认等在途请求完结；--force 立即终止）".to_string(),
            "  update prepare          Windows：手动更新前排空并关闭后台进程".to_string(),
            "  mcp                     以 MCP server（STDIO）运行，暴露 ask、whats_next、show_last、todo_add、todo_list 与 todo_update".to_string(),
            "  todo <子命令>            项目待办：add [--auto] [-f 路径] <文本> / list / attach / detach / rm / clear".to_string(),
            "  channel <子命令>         无 GUI 配置 IM 渠道（list/set/enable/disable/test/detect；见 'channel help'）".to_string(),
            "  agents <子命令>          Agent 状态与集成（monitor/show/install/uninstall/update；见 'agents help'）".to_string(),
            "  config <子命令>          通用配置键值兜底（show/get/set/unset/path；见 'config help'）".to_string(),
            "  doctor [--json]         一屏体检（daemon、渠道、集成）".to_string(),
            String::new(),
            "帮助:".to_string(),
            "  --agent-help            面向 AI 的精简提问用法".to_string(),
            "  --scripting-help        面向脚本/自动化的用法（严格选择、单选、JSON 输出）".to_string(),
            "  --help, -h              显示此帮助信息".to_string(),
            "  --version, -v           显示版本信息".to_string(),
        ]
        .join("\n"),
    }
}

/// 共享：提问参数说明（agent-help 用完整版；scripting-help 取其 `-q`/`-o` 等核心子集）。
fn ask_arg_lines(lang: Lang) -> Vec<String> {
    match lang {
        Lang::En => vec![
            "  <Message>             Shared description for all questions (optional)".to_string(),
            "  -f, --file <path>     Attach a file or image to the Message (absolute/relative/~); repeatable".to_string(),
            "  -q, --question <text> Ask a question; repeatable".to_string(),
            "  -o, --option <text>   Add a predefined answer option after a question".to_string(),
            "  -o!, --option! <text> Same as -o, and marks that option as your recommended answer".to_string(),
            "  --stdin               Read the <Message> from stdin via a quoted heredoc (write the heredoc last on the command line)".to_string(),
        ],
        Lang::Zh => vec![
            "  <Message>             所有问题的共享描述（可选）".to_string(),
            "  -f, --file <path>     为 Message 附带文件或图片（绝对/相对/~），可多次出现".to_string(),
            "  -q, --question <text> 提出问题，可多次出现".to_string(),
            "  -o, --option <text>   跟随在问题后，添加预定义回答选项".to_string(),
            "  -o!, --option! <text> 同 -o，并把该选项标记为你的推荐答案".to_string(),
            "  --stdin               用带引号的 heredoc 从标准输入读取 <Message>（heredoc 写在命令最后）".to_string(),
        ],
    }
}

/// 共享：严格选择 / 单选 / 输出格式开关（scripting-help 用；与 `--help` 提问块一致）。
fn script_flag_lines(lang: Lang) -> Vec<String> {
    match lang {
        Lang::En => vec![
            "  --single              Single choice instead of the default multiple choice".to_string(),
            "  --select-only         Choice only: forbid free text/attachments (every question must have -o options)".to_string(),
            "  --output <text|json>  Output format (default: text); json is recommended for scripts".to_string(),
        ],
        Lang::Zh => vec![
            "  --single              单选（默认多选）".to_string(),
            "  --select-only         严格选择：禁用自由文本/附件（每题必须有 -o 选项）".to_string(),
            "  --output <text|json>  输出格式（默认 text）；脚本建议用 json".to_string(),
        ],
    }
}

/// 共享：文本结果字段标记（取自 `output::MARKER_*`，保证与实际输出一致）。
fn result_field_lines(lang: Lang) -> Vec<String> {
    let m_opts = output::MARKER_SELECTED_OPTIONS;
    let m_input = output::MARKER_USER_INPUT;
    let m_files = output::MARKER_FILES;
    let m_status = output::MARKER_STATUS;
    match lang {
        Lang::En => vec![
            format!("  {m_opts}  Predefined options the user checked"),
            format!("  {m_input}        Free-form text the user typed"),
            format!("  {m_files}             Local paths the user attached (images/files/dirs; tell type by extension)"),
            format!("  {m_status}            State of this request (cancelled, unanswered, replayed answer); follow its text"),
        ],
        Lang::Zh => vec![
            format!("  {m_opts}  用户勾选的预定义选项"),
            format!("  {m_input}        用户输入的自由文本"),
            format!("  {m_files}             用户附带的本地路径（图片/文件/目录，按后缀区分类型）"),
            format!("  {m_status}            本次提问的状态（取消 / 未作答 / 重放的旧回答），按其中说明处理"),
        ],
    }
}

/// 共享：退出码（scripting-help 用）。
fn exit_code_lines(lang: Lang) -> Vec<String> {
    match lang {
        Lang::En => vec![
            "  0   The user answered, or cancelled (see action/status in the output)".to_string(),
            "  1   Invalid arguments or a runtime error (details on stderr)".to_string(),
        ],
        Lang::Zh => vec![
            "  0   用户已作答或已取消（结果中的 action/status 区分）".to_string(),
            "  1   参数错误或运行时错误（详情见 stderr）".to_string(),
        ],
    }
}

/// 面向 AI 的精简用法：调用方式、参数、文本结果区块与示例。
/// 结果标记取自 `output::MARKER_*`，确保「文档」与「实际输出」一致。
pub fn agent_help_text(lang: Lang) -> String {
    let prog = program_name();
    let mut out: Vec<String> = Vec::new();
    match lang {
        Lang::En => {
            out.push(format!("{prog} — ask a human and collect their response."));
            out.push(String::new());
            out.push("Invocation:".to_string());
            out.push(format!("  {prog} \"<Message>\" [-f \"<file>\" ...] [-q \"<question>\" [-o \"<option>\" ...] ...]"));
            out.push(String::new());
            out.push("Arguments:".to_string());
            out.extend(ask_arg_lines(lang));
            out.push(String::new());
            out.push("User response (returned only when present):".to_string());
            out.extend(result_field_lines(lang));
            out.push(String::new());
            out.push("Multi-question output:".to_string());
            out.push(
                "  Each question is grouped under \"# Qn\", with questions separated by \"---\""
                    .to_string(),
            );
            out.push(String::new());
            out.push("Examples:".to_string());
            out.push(format!(
                "  {prog} -q \"Proceed with deploy?\" -o! \"Proceed\" -o \"Stop\""
            ));
            out.push(format!("  {prog} \"Review this change?\" -f ./diff.patch -q \"Continue?\" -o \"Continue\" -o \"Stop\""));
            out.push(format!("  {prog} \"A few things to confirm\" -q \"Keep logs?\" -o \"Keep\" -o \"Clear\" -q \"Enable cache?\" -o \"On\" -o \"Off\""));
            // Heredoc must be last on the command line (put -q/-o before --stdin).
            out.push(format!(
                "  {prog} -q \"Continue?\" -o \"Continue\" -o \"Stop\" --stdin <<'EOF'"
            ));
            out.push(
                "# A long Markdown message with `backticks`, $vars and \"quotes\"".to_string(),
            );
            out.push("EOF".to_string());
            out.push(String::new());
            out.push("Project todos:".to_string());
            out.push("  Store a reminder for the user or a task they asked to do later; not your internal work plan.".to_string());
            out.push(
                "  Keep it to one actionable sentence, preferably no more than 100 characters."
                    .to_string(),
            );
            out.push(format!(
                "  Add when the user asks or defers a concrete task: {prog} todo add \"<task>\""
            ));
            out.push(format!(
                "  Attach files only when explicitly requested: {prog} todo add -f <path> -- \"<task>\"; use todo attach/detach for existing entries."
            ));
            out.push(String::new());
            out.push("End-of-task handoff (--whats-next):".to_string());
            out.push(format!(
                "  {prog} --whats-next [\"<completion report>\"] [-o[!] \"<suggested task>\" ...] [-f \"<file>\" ...] [--stdin]"
            ));
            out.push("  Run only after the current task is fully complete, to request a separate next task.".to_string());
            out.push(
                "  Use normal AskHuman questions for anything within the current task. The user"
                    .to_string(),
            );
            out.push(
                "  replies with the next task (start it immediately), or approves ending — only"
                    .to_string(),
            );
            out.push(
                "  then may you end it. -o/-o! are concrete next-task suggestions only; never add"
                    .to_string(),
            );
            out.push(
                "  an end/stop option because ending is built in. Omit them when there are no"
                    .to_string(),
            );
            out.push("  suggestions. Takes no -q (the question is fixed).".to_string());
            out.push(String::new());
            out.push("Context-compaction recovery:".to_string());
            out.push(format!(
                "  Run {prog} --show-last after summarization, or whenever the exact last AskHuman question/answer is uncertain."
            ));
        }
        Lang::Zh => {
            out.push(format!("{prog} —— 向人类发起提问并收集回应。"));
            out.push(String::new());
            out.push("调用方式:".to_string());
            out.push(format!("  {prog} \"<Message>\" [-f \"<文件>\" ...] [-q \"<问题>\" [-o \"<选项>\" ...] ...]"));
            out.push(String::new());
            out.push("参数说明:".to_string());
            out.extend(ask_arg_lines(lang));
            out.push(String::new());
            out.push("用户回应（仅在有内容时返回）:".to_string());
            out.extend(result_field_lines(lang));
            out.push(String::new());
            out.push("多问题输出:".to_string());
            out.push("  每题以「# Qn」分组，题与题之间用「---」分隔".to_string());
            out.push(String::new());
            out.push("使用示例:".to_string());
            out.push(format!(
                "  {prog} -q \"要继续部署吗？\" -o! \"继续\" -o \"停止\""
            ));
            out.push(format!("  {prog} \"看看这个改动？\" -f ./diff.patch -q \"要继续吗？\" -o \"继续\" -o \"停止\""));
            out.push(format!("  {prog} \"以下是几处待确认\" -q \"保留日志？\" -o \"保留\" -o \"清除\" -q \"开启缓存？\" -o \"开\" -o \"关\""));
            // heredoc 必须写在命令最后（先 -q/-o，再 --stdin）。
            out.push(format!(
                "  {prog} -q \"要继续吗？\" -o \"继续\" -o \"停止\" --stdin <<'EOF'"
            ));
            out.push("# 含 `反引号`、$VAR 与 \"引号\" 的长 Markdown 消息".to_string());
            out.push("EOF".to_string());
            out.push(String::new());
            out.push("项目待办:".to_string());
            out.push(
                "  用于提醒用户操作，或记录用户要求稍后执行的任务；不是 Agent 的内部工作计划。"
                    .to_string(),
            );
            out.push("  建议写成一个可执行的句子，尽量不超过 100 个字符。".to_string());
            out.push(format!(
                "  用户要求添加或明确延后具体任务时使用：{prog} todo add \"<任务>\""
            ));
            out.push(format!(
                "  仅在用户明确要求时添加文件：{prog} todo add -f <路径> -- \"<任务>\"；已有条目用 todo attach/detach。"
            ));
            out.push(String::new());
            out.push("任务完成后的交接（--whats-next）:".to_string());
            out.push(format!(
                "  {prog} --whats-next [\"<完成报告>\"] [-o[!] \"<建议任务>\" ...] [-f \"<文件>\" ...] [--stdin]"
            ));
            out.push(
                "  仅在当前任务完全完成后运行，用于领取一个独立的下一任务。当前任务内的任何"
                    .to_string(),
            );
            out.push(
                "  问题都用普通 AskHuman 提问。用户会给出下一个任务（立即开始执行），或确认"
                    .to_string(),
            );
            out.push(
                "  结束——仅此时才可结束。-o/-o! 只放具体的下一任务建议；不要添加结束/停止"
                    .to_string(),
            );
            out.push("  选项，因为结束项已内置。无建议时省略。不接受 -q（问题固定）。".to_string());
            out.push(String::new());
            out.push("上下文压缩恢复:".to_string());
            out.push(format!(
                "  被摘要后，或不确定上一次 AskHuman 问答的精确内容时，运行 {prog} --show-last。"
            ));
        }
    }
    out.join("\n")
}

/// 面向脚本/自动化的用法：强调严格选择、单选、JSON 输出与退出码。
/// 与 `agent_help_text` 共用 `ask_arg_lines`/`script_flag_lines`/`exit_code_lines` 片段。
pub fn scripting_help_text(lang: Lang) -> String {
    let prog = program_name();
    let mut out: Vec<String> = Vec::new();
    match lang {
        Lang::En => {
            out.push(format!(
                "{prog} — collect a structured choice from a human, for scripts/automation."
            ));
            out.push(String::new());
            out.push("Invocation:".to_string());
            out.push(format!("  {prog} \"<Message>\" -q \"<question>\" -o \"<option>\" ... [--single] [--select-only] [--output json]"));
            out.push(String::new());
            out.push("Asking arguments (same as --agent-help):".to_string());
            out.extend(ask_arg_lines(lang));
            out.push(String::new());
            out.push("Scripting options:".to_string());
            out.extend(script_flag_lines(lang));
            out.push(String::new());
            out.push("Exit codes:".to_string());
            out.extend(exit_code_lines(lang));
            out.push(String::new());
            out.push("JSON output (--output json):".to_string());
            out.push(
                "  action   \"answer\" when the user responded, \"cancel\" when they cancelled"
                    .to_string(),
            );
            out.push(
                "  channel  Which channel the response came from (popup/slack/feishu/...)"
                    .to_string(),
            );
            out.push(
                "  answers  Present only for \"answer\"; one entry per ANSWERED question:"
                    .to_string(),
            );
            out.push("    question_index    0-based index of the question".to_string());
            out.push(
                "    selected_options  Option texts the user picked (single choice → exactly one)"
                    .to_string(),
            );
            out.push("    selected_indices  0-based indices of those options".to_string());
            out.push(
                "    user_input        Free text (omitted under --select-only or when empty)"
                    .to_string(),
            );
            out.push("    files             Local paths attached (omitted when none)".to_string());
            out.push("  Empty/omitted fields and unanswered questions are dropped to keep the JSON small.".to_string());
            out.push(String::new());
            out.push("Example:".to_string());
            out.push(format!("  {prog} -q \"Deploy target?\" -o \"staging\" -o! \"production\" --single --select-only --output json"));
        }
        Lang::Zh => {
            out.push(format!("{prog} —— 面向脚本/自动化，收集结构化的人类选择。"));
            out.push(String::new());
            out.push("调用方式:".to_string());
            out.push(format!("  {prog} \"<Message>\" -q \"<问题>\" -o \"<选项>\" ... [--single] [--select-only] [--output json]"));
            out.push(String::new());
            out.push("提问参数（同 --agent-help）:".to_string());
            out.extend(ask_arg_lines(lang));
            out.push(String::new());
            out.push("脚本选项:".to_string());
            out.extend(script_flag_lines(lang));
            out.push(String::new());
            out.push("退出码:".to_string());
            out.extend(exit_code_lines(lang));
            out.push(String::new());
            out.push("JSON 输出（--output json）:".to_string());
            out.push("  action   用户作答为 \"answer\"，取消为 \"cancel\"".to_string());
            out.push("  channel  回应来自哪个渠道（popup/slack/feishu/...）".to_string());
            out.push("  answers  仅 \"answer\" 时出现；每个「已作答」的问题一条:".to_string());
            out.push("    question_index    问题的 0 基下标".to_string());
            out.push("    selected_options  用户选择的选项原文（单选时恰好一个）".to_string());
            out.push("    selected_indices  这些选项的 0 基下标".to_string());
            out.push("    user_input        自由文本（--select-only 或为空时省略）".to_string());
            out.push("    files             附带的本地路径（无则省略）".to_string());
            out.push("  空字段与未作答的问题都会被省略，以减小 JSON 体积。".to_string());
            out.push(String::new());
            out.push("示例:".to_string());
            out.push(format!("  {prog} -q \"部署目标？\" -o \"staging\" -o! \"production\" --single --select-only --output json"));
        }
    }
    out.join("\n")
}

pub fn version_text() -> String {
    format!("AskHuman v{}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_basename_from_path() {
        assert_eq!(
            program_name_from(Some("/usr/local/bin/AskHuman")),
            "AskHuman"
        );
        assert_eq!(program_name_from(Some("./AskHuman")), "AskHuman");
    }

    #[test]
    fn keeps_name_without_separator() {
        assert_eq!(program_name_from(Some("AskHuman")), "AskHuman");
        assert_eq!(program_name_from(Some("wblra ask")), "wblra ask");
    }

    #[test]
    fn falls_back_when_missing_or_empty() {
        assert_eq!(program_name_from(None), "AskHuman");
        assert_eq!(program_name_from(Some("")), "AskHuman");
        assert_eq!(program_name_from(Some("   ")), "AskHuman");
    }

    #[test]
    fn help_localized_by_lang() {
        let en = help_text(Lang::En);
        assert!(en.contains("<message>"));
        assert!(en.contains("Human-in-the-loop interaction tool"));
        let zh = help_text(Lang::Zh);
        assert!(zh.contains("用法:"));
    }

    #[test]
    fn agent_help_markers_match_output() {
        // agent-help 文档里的标记必须与 output.rs 实际输出常量一致（恒英文）。
        for lang in [Lang::En, Lang::Zh] {
            let h = agent_help_text(lang);
            assert!(h.contains(output::MARKER_SELECTED_OPTIONS));
            assert!(h.contains(output::MARKER_USER_INPUT));
            assert!(h.contains(output::MARKER_FILES));
            assert!(h.contains(output::MARKER_STATUS));
        }
    }

    #[test]
    fn scripting_help_covers_script_flags_and_json() {
        for lang in [Lang::En, Lang::Zh] {
            let h = scripting_help_text(lang);
            assert!(h.contains("--select-only"));
            assert!(h.contains("--single"));
            assert!(h.contains("--output"));
            assert!(h.contains("question_index"));
            assert!(h.contains("selected_indices"));
        }
    }

    #[test]
    fn help_has_three_sections() {
        let en = help_text(Lang::En);
        assert!(en.contains("Asking"));
        assert!(en.contains("Management:"));
        assert!(en.contains("--scripting-help"));
    }

    #[test]
    fn help_and_agent_help_cover_whats_next_and_todo() {
        // spec todo-whats-next D4/D6：--help 列出 --whats-next 与 todo 子命令；
        // --agent-help 只说用法与语义，不描述输出结构（第 19 轮定案：复用 Ask 标准区块）。
        for lang in [Lang::En, Lang::Zh] {
            let h = help_text(lang);
            assert!(h.contains("--whats-next"));
            assert!(h.contains("todo "));
            let ah = agent_help_text(lang);
            assert!(ah.contains("--whats-next"));
            assert!(ah.contains("todo add"));
            // 旧「固定英文结束句」已废除，不应再出现在 help 里。
            assert!(!ah.contains("no more tasks"));
        }
    }

    #[test]
    fn help_and_agent_help_cover_context_recovery_in_both_languages() {
        for lang in [Lang::En, Lang::Zh] {
            let help = help_text(lang);
            let agent = agent_help_text(lang);
            assert!(help.contains("--show-last"));
            assert!(help.contains("show_last"));
            assert!(agent.contains("--show-last"));
        }
    }

    #[test]
    fn agent_help_distinguishes_project_todos_from_internal_plans() {
        let en = agent_help_text(Lang::En);
        assert!(en.contains("user asks or defers a concrete task"));
        assert!(en.contains("not your internal work plan"));
        assert!(en.contains("one actionable sentence"));
        assert!(en.contains("no more than 100 characters"));

        let zh = agent_help_text(Lang::Zh);
        assert!(zh.contains("用户要求添加或明确延后具体任务时使用"));
        assert!(zh.contains("不是 Agent 的内部工作计划"));
        assert!(zh.contains("一个可执行的句子"));
        assert!(zh.contains("不超过 100 个字符"));
    }

    #[test]
    fn agent_help_frames_whats_next_as_end_of_task_handoff() {
        let en = agent_help_text(Lang::En);
        assert!(en.contains("End-of-task handoff (--whats-next)"));
        assert!(en.contains("only after the current task is fully complete"));
        assert!(en.contains("never add\n  an end/stop option because ending is built in"));

        let zh = agent_help_text(Lang::Zh);
        assert!(zh.contains("任务完成后的交接（--whats-next）"));
        assert!(zh.contains("仅在当前任务完全完成后运行"));
        assert!(zh.contains("不要添加结束/停止\n  选项，因为结束项已内置"));
    }
}
