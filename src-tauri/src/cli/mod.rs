pub mod agents_cmd;
pub mod args;
pub mod cfgio;
pub mod channel_cmd;
pub mod config_cmd;
pub mod debug_cmd;
pub mod dev_cmd;
pub mod doctor;
pub mod file_attachment;
pub mod help;
pub mod image_writer;
pub mod output;
pub mod todo_cmd;
pub mod update_cmd;

use crate::i18n::{self, Lang};
use std::collections::HashMap;
use std::process::exit;

pub(crate) const FROM_MCP_ENV: &str = "ASKHUMAN_FROM_MCP";
pub(crate) const MCP_INSTANCE_ID_ENV: &str = "ASKHUMAN_MCP_INSTANCE_ID";
pub(crate) const MCP_AGENT_KIND_ENV: &str = "ASKHUMAN_MCP_AGENT_KIND";
pub(crate) const MCP_AGENT_SESSION_ID_ENV: &str = "ASKHUMAN_MCP_AGENT_SESSION_ID";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CallerContext {
    pub agent_kind: Option<String>,
    pub agent_session_id: Option<String>,
    pub mcp_instance_id: Option<String>,
    pub from_mcp: bool,
}

/// 向 stdout 输出一行文本，并把 BrokenPipe（读端提前关闭，如 `AskHuman --agent-help | head`）
/// 视为正常结束：写失败一律静默忽略，退出码由调用方决定（纯输出命令随后 exit(0)，错误分支 exit(1)）。
///
/// 背景：Rust 运行时默认把 SIGPIPE 设为忽略，写已关闭管道返回 EPIPE 而非被信号终止；若用
/// `println!`，写失败会 panic，而 release 为 `panic = "abort"`，最终以退出码 134 退出。改用本函数规避。
fn print_line(text: &str) {
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{text}").and_then(|_| out.flush());
}

/// 入口分发：在创建任何窗口前按 argv 分流。
pub fn dispatch() {
    // Dev Instance: pin ASKHUMAN_HOME / re-exec worktree bin before any config or GUI load.
    crate::dev_instance::maybe_enter_dev_instance();

    let argv: Vec<String> = std::env::args().collect();
    let lang = Lang::current();

    // 完全无参数：报错 + 通用 Help（让用户直接 `AskHuman` 即可看到全部用法，而非仅提问说明）。
    // 注意：有参数但解析失败 / 未知选项的情况，仍展示提问导向的 agent-help（见下方分支）。
    if argv.len() < 2 {
        eprintln!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "cli.missingContent")
        );
        eprintln!(
            "{}\n",
            i18n::tr(lang, "cli.seeAgentHelp").replace("{prog}", &help::program_name())
        );
        print_line(&help::help_text(lang));
        exit(1);
    }

    match argv[1].as_str() {
        "--help" | "-h" => {
            print_line(&help::help_text(lang));
            exit(0);
        }
        "--version" | "-v" => {
            print_line(&help::version_text());
            exit(0);
        }
        "--agent-help" => {
            print_line(&help::agent_help_text(lang));
            exit(0);
        }
        "--show-last" => {
            let count = match argv.get(2).map(String::as_str) {
                None => 1usize,
                Some(raw) if argv.len() == 3 => match crate::show_last::parse_count(Some(raw)) {
                    Ok(n) => n,
                    Err(error) => {
                        eprintln!("{}{error}", i18n::err_prefix(lang));
                        exit(1);
                    }
                },
                _ => {
                    eprintln!(
                        "{}{}",
                        i18n::err_prefix(lang),
                        crate::show_last::Error::InvalidCount
                    );
                    exit(1);
                }
            };
            let context = caller_context();
            let transcript = match (&context.agent_kind, &context.agent_session_id) {
                (Some(agent_kind), Some(session_id)) => Some(crate::show_last::TranscriptHint {
                    agent_kind: agent_kind.clone(),
                    session_id: session_id.clone(),
                }),
                _ => None,
            };
            let scope = match show_last_cli_scope(
                context.agent_kind,
                context.agent_session_id,
                crate::project::detect(),
            ) {
                Ok(scope) => scope,
                Err(error) => {
                    eprintln!("{}{error}", i18n::err_prefix(lang));
                    exit(1);
                }
            };
            match crate::show_last::recover(
                &scope,
                count,
                crate::show_last::Surface::Cli,
                transcript.as_ref(),
            ) {
                Ok(output) => {
                    print_line(&output);
                    exit(0);
                }
                Err(error) => {
                    eprintln!("{}{error}", i18n::err_prefix(lang));
                    exit(1);
                }
            }
        }
        "--scripting-help" => {
            print_line(&help::scripting_help_text(lang));
            exit(0);
        }
        // 设置/历史窗口只需 general(主题)；密钥的「已保存」判定由前端 `get_settings` 单独读取。
        // 故用 load_without_secrets()，避免打开这两个窗口时无谓读钥匙串。
        // Route every desktop platform through the unified GUI Host (global single windows).
        // 失败（极端：宿主起不来）兜底本进程直接建窗，保证窗口至少能打开。
        "--settings" => {
            if crate::gui_host::host_open(
                crate::gui_host::WindowKind::Settings,
                false,
                None,
                None,
                None,
            )
            .is_ok()
            {
                exit(0);
            }
            crate::app::run_settings(crate::config::AppConfig::load_without_secrets());
        }
        // 独立历史窗口：默认当前项目（向上找 .git 根、回退 cwd）；`--all` 默认展示全部项目。
        "--history" => {
            let all = argv[2..].iter().any(|a| a == "--all");
            // 项目过滤随请求经宿主 IPC 传递（宿主自身 cwd 无意义）。
            let project = crate::project::detect();
            if crate::gui_host::host_open(
                crate::gui_host::WindowKind::History,
                all,
                Some(project),
                None,
                None,
            )
            .is_ok()
            {
                exit(0);
            }
            crate::app::run_history(
                crate::project::detect(),
                all,
                crate::config::AppConfig::load_without_secrets(),
            );
        }
        // 独立待办窗口：预选当前项目（与 `--history` 同探测规则）；窗内仍可切换项目。
        "--todos" => {
            let project = crate::project::detect();
            if crate::gui_host::host_open(
                crate::gui_host::WindowKind::Todos,
                false,
                Some(project.clone()),
                None,
                None,
            )
            .is_ok()
            {
                exit(0);
            }
            crate::app::run_todos(project, crate::config::AppConfig::load_without_secrets());
        }
        // 隐藏的统一 GUI 宿主角色（spec D2）：单实例托盘 + 设置/历史/Agent 窗口宿主。
        // 由 CLI 路由 / daemon 按需 spawn；抢宿主单实例锁失败即直接退出（已有宿主在跑）。
        "--gui-host" => {
            crate::app::run_gui_host(crate::config::AppConfig::load_without_secrets());
        }
        // 隐藏的 GUI Helper 角色：由 Daemon spawn（`--popup --endpoint <sock> --token <tok>`）。
        "--popup" => {
            let mut endpoint = String::new();
            let mut token = String::new();
            // 方案6：预热模式由 daemon 以 `--popup --warm` 拉起（无 token），先建窗挂载、隐藏待命，
            // 入 daemon「热池」，来请求时由 daemon 喂 Show 领用上屏。
            let mut warm = false;
            let mut i = 2;
            while i < argv.len() {
                match argv[i].as_str() {
                    "--endpoint" if i + 1 < argv.len() => {
                        endpoint = argv[i + 1].clone();
                        i += 2;
                    }
                    "--token" if i + 1 < argv.len() => {
                        token = argv[i + 1].clone();
                        i += 2;
                    }
                    "--warm" => {
                        warm = true;
                        i += 1;
                    }
                    _ => i += 1,
                }
            }
            crate::app::run_gui_helper(endpoint, token, warm);
        }
        // Dev Instance：enable/disable/status/preset（多 WorkTree 并行开发隔离）。
        "dev" => {
            dev_cmd::dispatch(&argv[2..], lang);
        }
        // 常驻 Daemon 管理子命令：AskHuman daemon <run|start|stop|restart|status|logs>。
        // 极端歧义（问题正好是 "daemon"）可用 `AskHuman -q daemon` 规避。
        "daemon" => {
            crate::daemon::dispatch(&argv[2..]);
        }
        // Manual Windows update preparation: gracefully drain the daemon and close the GUI Host
        // so the user can replace the running executable from their terminal.
        "update" => {
            exit(update_cmd::dispatch(&argv[2..]));
        }
        // MCP server 角色：以 STDIO 暴露 ask / whats_next / todo_add，供 Codex / Claude Code / Cursor 等 MCP 客户端调用。
        // 每次工具调用都 spawn 一个 `AskHuman --output json …` 子进程复用既有 ask 流程（见 mcp 模块）。
        // 极端歧义（问题正好是 "mcp"）可用 `AskHuman -q mcp` 规避。
        "mcp" => {
            crate::mcp::run();
        }
        // 隐藏的生命周期上报器：由三家 Agent 的用户级 hook 调用
        // （`AskHuman __agent-hook <agent> <event>`，spec D20）。即发即走、静默退出。
        "__agent-hook" => {
            crate::agents::report::run(&argv[2..]);
            exit(0);
        }
        // Hidden context-compaction/session-binding hook. It is mode-owned and separate from the
        // optional lifecycle capability inside the same automatic integration.
        "__context-recovery-hook" => {
            crate::agents::context_recovery::run(&argv[2..]);
            exit(0);
        }
        // Hidden SubagentStart context hook for Claude Code and Codex.
        "__subagent-hook" => {
            if let Some(output) = crate::integrations::agent_subagent_guard::hook_output(
                argv.get(2).map(String::as_str),
            ) {
                print_line(&output);
            }
            exit(0);
        }
        // Hidden one-time bridge used only by a newly opened platform terminal.
        "__agent-launch" => {
            if let Err(error) = crate::integrations::agent_launch::run_helper(&argv[2..]) {
                eprintln!("AskHuman: {error:#}");
                exit(1);
            }
            exit(0);
        }
        // Hidden transactional Windows self-update worker.
        "__update-worker" => {
            if let Err(error) = crate::update::direct::run_windows_worker(&argv[2..]) {
                eprintln!("AskHuman update worker: {error:#}");
                exit(1);
            }
            exit(0);
        }
        // Hidden Windows npm updater copied outside the package being replaced.
        "__npm-update-worker" => {
            if let Err(error) = crate::update::npm::run_windows_worker(&argv[2..]) {
                eprintln!("AskHuman npm update worker: {error:#}");
                exit(1);
            }
            exit(0);
        }
        // Hidden Stop confirmation hook. Failures emit `{}` so the agent can stop normally.
        "__stop-hook" => {
            crate::agents::stop::run(&argv[2..]);
            exit(0);
        }
        // Hidden PermissionRequest adapter. All infrastructure and validation failures produce no
        // stdout so the agent falls back to its native approval prompt.
        "__permission-hook" => {
            if let Some(output) = crate::permissions::run(argv.get(2).map(String::as_str)) {
                print_line(&output);
            }
            exit(0);
        }
        // Hidden PreToolUse adapter for Claude's built-in AskUserQuestion: the questions are
        // answered through AskHuman instead of Claude's own picker.
        "__ask-question-hook" => {
            crate::ask_question::run(argv.get(2).map(String::as_str));
            exit(0);
        }
        // Hidden short-lived file snapshot worker used only by the local permission popup.
        "__permission-diff-worker" => {
            if let Some(output) = crate::permission_diff::worker::run_stdio() {
                print_line(&output);
            }
            exit(0);
        }
        // Hidden short-lived shell policy analysis worker (codex-permission-remember D27).
        "__permission-shell-worker" => {
            if let Some(output) = crate::permission_shell::run_stdio() {
                print_line(&output);
            }
            exit(0);
        }
        // Agent 状态 + 集成子命令组（spec：cli-config）：monitor / show / install / uninstall / update。
        "agents" => {
            agents_cmd::dispatch(&argv[2..], lang);
        }
        // IM 渠道配置（headless / 无 GUI）：list / set / enable / disable / test / detect。
        "channel" => {
            channel_cmd::dispatch(&argv[2..], lang);
        }
        // 通用键值兜底：show / get / set / unset / path。
        "config" => {
            config_cmd::dispatch(&argv[2..], lang);
        }
        // 一屏体检：daemon / 渠道 / 集成。
        "doctor" => {
            doctor::dispatch(&argv[2..], lang);
            exit(0);
        }
        // 项目级待办队列（spec todo-whats-next D6）：add / list / rm / clear。
        // 极端歧义（问题正好是 "todo"）可用 `AskHuman -q todo` 规避。
        "todo" => {
            todo_cmd::dispatch(&argv[2..], lang);
        }
        // 隐藏调试子命令组（不进 help）：如钉钉 watch PoC 探针 `debug dd-watch-poc`。
        "debug" => {
            debug_cmd::dispatch(&argv[2..], lang);
            exit(0);
        }
        // 第一题既可用位置参数，也可用 `-q`/`--question`；提问相关 flag 一律进入提问分支，
        // 由 `parse_ask` 给出精确错误（如缺少问题内容、选项需在问题之后）。
        first
            if first.starts_with('-')
                && !matches!(
                    first,
                    "-q" | "--question"
                        | "-o"
                        | "--option"
                        | "-o!"
                        | "--option!"
                        | "-f"
                        | "--file"
                        | "--stdin"
                        | "--select-only"
                        | "--single"
                        | "--output"
                        | "--whats-next"
                ) =>
        {
            eprintln!(
                "{}{}\n",
                i18n::err_prefix(lang),
                i18n::tr(lang, "cli.unknownOption").replace("{opt}", first)
            );
            print_line(&help::agent_help_text(lang));
            exit(1);
        }
        _ => match parse_ask_with_stdin(&argv[1..], lang) {
            Ok(parsed) => {
                // 解析 Message 的展示附件（-f 始终归 Message）。
                let files = match file_attachment::resolve(&parsed.message_files, lang) {
                    Ok(files) => files,
                    Err(e) => {
                        eprintln!("{}{}", i18n::err_prefix(lang), e);
                        exit(1);
                    }
                };
                let message = crate::models::MessagePrompt::new(parsed.message_text, files);
                // 项目 key（git 根，回退 cwd）：whats-next 取待办 + TaskRequest 归属共用。
                let project = crate::project::detect();
                // 自动执行待办（第 17 轮定案）：whats-next 且存在自动待办 → 不发卡提问，直接
                // 出队最靠前的一条并打印其原文（agent 把它当作下一个任务）；完成报告照常落回复
                // 历史。被并发拿走（竞态）→ 回落正常提问。Stop 卡不走此路径。
                if parsed.whats_next && try_whats_next_auto(&project, &message, lang) {
                    return;
                }
                // whats-next (spec D2): fixed question + suggestions + todo chips + a final end
                // option. Auto-run todo takeover already happened above and keeps its priority.
                let questions: Vec<crate::models::Question> = if parsed.whats_next {
                    vec![whats_next_question(
                        &project,
                        &parsed.whats_next_options,
                        lang,
                    )]
                } else {
                    parsed
                        .questions
                        .into_iter()
                        .map(|q| {
                            let options = q
                                .options
                                .into_iter()
                                .map(|o| crate::models::OptionItem::new(o.text, o.recommended))
                                .collect();
                            crate::models::Question::new(q.message, options)
                        })
                        .collect()
                };
                // Thin client through the shared daemon + GUI Helper on every desktop platform.
                // 性能埋点（spec popup-launch-performance §7）：仅 `ASKHUMAN_PERF` 开启时铸 id，
                // 经 TaskRequest 透传到 daemon/helper/前端串联整条时间线；关闭则恒空、零开销。
                let perf_id = if crate::perf::enabled() {
                    format!("{}-{}", std::process::id(), crate::perf::now_ms())
                } else {
                    String::new()
                };
                crate::perf::mark_at(&perf_id, "cli.start", crate::perf::start_ms());
                // harness 注入的 spawn 时刻（含进程创建 / 加载，main 之前不可见的开销）。
                crate::perf::mark_spawn(&perf_id);
                // 顺带探测调用方 Agent 身份（生命周期追踪 spec D21）：仅 env 读取（家族 + 会话 ID，零 ps）。
                // 方案5(b)：进程树 walk（数十 ms 的 ps 游走）移到 daemon 异步进行——这里只带 CLI 自身 pid。
                let context = caller_context();
                crate::perf::mark(&perf_id, "cli.detect_done");
                // 来源名解析：未定制 `ASKHUMAN_ENV_SOURCE_NAME` 时，用探测到的 Agent 名
                // （Claude Code / Codex / Cursor）替代默认 "the Loop"；供渠道消息头 + 历史共用
                // （弹窗标题另由前端按胶囊内联渲染）。MCP 模式 env 判不出家族 → 回退 "the Loop"。
                let resolved_agent_kind = context
                    .agent_kind
                    .as_deref()
                    .and_then(crate::agents::AgentKind::parse);
                let task = crate::ipc::TaskRequest {
                    message,
                    questions,
                    // Markdown 渲染恒开（`--no-markdown` 已移除）；弹窗内可临时切换为源码视图。
                    is_markdown: true,
                    source: crate::models::source_name_for_agent(resolved_agent_kind),
                    lang: lang.code().to_string(),
                    project,
                    select_only: parsed.select_only,
                    single: parsed.single,
                    output_format: parsed.output_format,
                    record_history: true,
                    agent_kind: context.agent_kind,
                    agent_session_id: context.agent_session_id,
                    mcp_instance_id: context.mcp_instance_id,
                    agent_pid: None,
                    caller_pid: std::process::id(),
                    from_mcp: context.from_mcp,
                    perf_id,
                    perf_autodismiss: crate::perf::autodismiss(),
                    whats_next: parsed.whats_next,
                };
                crate::client::run_ask(task);
            }
            Err(e) => {
                eprintln!("{}{}\n", i18n::err_prefix(lang), e);
                print_line(&help::agent_help_text(lang));
                exit(1);
            }
        },
    }
}

/// whats-next 自动接管（第 17 轮定案）：出队最靠前的自动待办并直接打印其文本；成功返回 true。
/// 完成报告（Message）落回复历史——自动路径没有卡片可展示，历史窗口是唯一可查处。
fn try_whats_next_auto(project: &str, message: &crate::models::MessagePrompt, lang: Lang) -> bool {
    let Some(entry) = crate::todos::first_auto(project) else {
        return false;
    };
    let delivery_request_id = uuid::Uuid::new_v4().to_string();
    let expected: Vec<_> = entry.attachments.iter().map(|a| a.snapshot()).collect();
    let delivery = crate::todos::prepare_delivery_consistent(
        project,
        &entry.id,
        &expected,
        &delivery_request_id,
    );
    // 出队即历史记录点（take 落待办执行历史）；被并发拿走 → 回落正常提问。
    let Some(entry) = crate::todos::take(project, std::slice::from_ref(&entry.id))
        .into_iter()
        .next()
    else {
        crate::todo_attachments::cleanup_delivery(&delivery_request_id);
        return false;
    };
    let limit = crate::config::AppConfig::load_without_secrets()
        .general
        .history_limit;
    if limit > 0 {
        let context = caller_context();
        let resolved = context
            .agent_kind
            .as_deref()
            .and_then(crate::agents::AgentKind::parse);
        let prefix = i18n::tr(lang, "whatsNext.todoPrefix");
        crate::history::record(
            crate::history::HistoryEntry {
                id: uuid::Uuid::new_v4().to_string(),
                timestamp_ms: crate::history::now_ms(),
                project: project.to_string(),
                source: crate::models::source_name_for_agent(resolved),
                agent_kind: context.agent_kind,
                agent_session_id: context.agent_session_id,
                mcp_instance_id: context.mcp_instance_id,
                channel: "auto".to_string(),
                action: crate::models::ChannelAction::Send,
                is_markdown: true,
                message: message.clone(),
                questions: vec![crate::models::Question::new(
                    i18n::tr(lang, "whatsNext.question").to_string(),
                    Vec::new(),
                )],
                answers: vec![crate::history::HistoryAnswer {
                    selected_options: vec![format!("{}{}", prefix, entry.text)],
                    user_input: None,
                    images: Vec::new(),
                    files: delivery.files.clone(),
                }],
            },
            limit,
        );
    }
    // 与人工路径同构（第 19 轮定案：复用 Ask 标准区块）：派活 → `[user_input]` + 任务文本。
    let task = match crate::todo_attachments::warning_block(&delivery.warnings) {
        Some(warning) => format!("{}\n\n{warning}", entry.text),
        None => entry.text.clone(),
    };
    print_line(&crate::cli::output::whats_next_output(
        &crate::cli::output::WhatsNextReply::Task(task),
        &delivery.files,
        lang,
    ));
    true
}

/// Total whats-next option limit, including suggestions, todos, and the final end option.
const WHATS_NEXT_MAX_OPTIONS: usize = 10;

/// Normalize agent suggestion text for end-option detection (see [`crate::textnorm`]).
fn normalize_whats_next_option_key(text: &str) -> String {
    crate::textnorm::normalize_key(text)
}

/// Agent-supplied end-ish labels (normalized keys). Built-in i18n end strings are always
/// checked separately for both languages. Whole-key equality only — longer real tasks
/// that merely contain these words (e.g. "结束本轮的文档") are kept.
const SPURIOUS_WHATS_NEXT_END_KEYS: &[&str] = &[
    // Chinese
    "结束本轮",
    "结束对话",
    "结束会话",
    "结束",
    "收工",
    "没有更多",
    "没有更多任务",
    "没有更多工作",
    "无事可做",
    "没事了",
    "没有了",
    "先这样",
    "不用了",
    "就到这里",
    "可以结束了",
    // English
    "endthisturn",
    "endtheturn",
    "endturn",
    "endthissession",
    "endsession",
    "endconversation",
    "endthisconversation",
    "nomore",
    "nomorework",
    "nomoretasks",
    "nomoretodos",
    "nothingelse",
    "nothingtodo",
    "alldone",
    "weredone",
    "wearedone",
    "nofurtherwork",
    "nofurthertasks",
    "stop",
    "done",
    "finish",
    "finished",
];

/// True when a caller `-o` / MCP option is a mistaken "end this turn" stand-in and must be
/// dropped so only AskHuman's built-in end option remains.
fn is_spurious_whats_next_end_option(text: &str) -> bool {
    let key = normalize_whats_next_option_key(text);
    if key.is_empty() {
        return false;
    }
    if SPURIOUS_WHATS_NEXT_END_KEYS.contains(&key.as_str()) {
        return true;
    }
    // Built-in labels in both UI languages (agent lang may disagree with current UI lang).
    for lang in [Lang::Zh, Lang::En] {
        if key == normalize_whats_next_option_key(i18n::tr(lang, "whatsNext.endOption")) {
            return true;
        }
    }
    false
}

/// Build the fixed whats-next question (spec todo-whats-next D2): `-o`/`-o!` suggestions first,
/// project todo chips next, and the end option last. Suggestions consume the ten-option capacity
/// first and are silently truncated; todos fill the remainder in FIFO order. Auto-run takeover
/// happens before this function is called.
///
/// Caller suggestions that look like an end/stop/no-more-work choice are dropped: the protocol
/// forbids agents from supplying those; AskHuman always appends the sole built-in end option.
fn whats_next_question(
    project: &str,
    suggestions: &[args::OptArg],
    lang: Lang,
) -> crate::models::Question {
    whats_next_question_from_entries(suggestions, crate::todos::list(project), lang)
}

fn whats_next_question_from_entries(
    suggestions: &[args::OptArg],
    entries: Vec<crate::todos::TodoEntry>,
    lang: Lang,
) -> crate::models::Question {
    let total = entries.len();
    let task_slots = WHATS_NEXT_MAX_OPTIONS - 1;
    let mut options: Vec<crate::models::OptionItem> = suggestions
        .iter()
        .filter(|option| !is_spurious_whats_next_end_option(&option.text))
        .take(task_slots)
        .map(|option| crate::models::OptionItem::new(&option.text, option.recommended))
        .collect();
    let todo_slots = task_slots - options.len();
    let shown_todos = total.min(todo_slots);
    options.extend(entries.into_iter().take(todo_slots).map(|entry| {
        crate::models::OptionItem::with_todo_entry(crate::todos::option_label(lang, &entry), &entry)
    }));
    options.push(crate::models::OptionItem::new(
        i18n::tr(lang, "whatsNext.endOption"),
        false,
    ));
    let mut message = i18n::tr(lang, "whatsNext.question").to_string();
    // When suggestions consume every task slot, omit overflow noise as explicitly requested.
    if todo_slots > 0 && total > shown_todos {
        let note =
            i18n::tr(lang, "todo.moreNote").replace("{n}", &(total - shown_todos).to_string());
        message.push_str("\n\n");
        message.push_str(&note);
    }
    crate::models::Question::new(message, options)
}

/// Resolve the calling Agent with environment-only work so it is safe on the ask hot path and
/// available to daemon-backed Unix and Windows builds.
pub(crate) fn caller_context() -> CallerContext {
    caller_context_from_env(&std::env::vars().collect())
}

fn caller_context_from_env(env: &HashMap<String, String>) -> CallerContext {
    let nonempty = |name: &str| {
        env.get(name)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let from_mcp = nonempty(FROM_MCP_ENV).is_some_and(|value| value != "0");
    if from_mcp {
        let kind = nonempty(MCP_AGENT_KIND_ENV)
            .filter(|value| crate::agents::AgentKind::parse(value).is_some());
        let session_id = nonempty(MCP_AGENT_SESSION_ID_ENV);
        let (agent_kind, agent_session_id) = match (kind, session_id) {
            (Some(kind), Some(session_id)) => (Some(kind), Some(session_id)),
            _ => (None, None),
        };
        return CallerContext {
            agent_kind,
            agent_session_id,
            mcp_instance_id: nonempty(MCP_INSTANCE_ID_ENV),
            from_mcp: true,
        };
    }
    let kind = crate::agents::detect::detect_running_agent_from(env);
    CallerContext {
        agent_kind: kind.map(|kind| kind.as_str().to_string()),
        agent_session_id: kind
            .and_then(|kind| crate::agents::detect::session_id_from_env_map(kind, env)),
        mcp_instance_id: None,
        from_mcp: false,
    }
}

fn show_last_cli_scope(
    agent_kind: Option<String>,
    agent_session_id: Option<String>,
    project: String,
) -> Result<crate::show_last::Scope, &'static str> {
    match (agent_kind, agent_session_id) {
        (Some(agent_kind), Some(session_id)) => Ok(crate::show_last::Scope::AgentSession {
            agent_kind,
            session_id,
        }),
        (None, None) => Ok(crate::show_last::Scope::Project(project)),
        _ => Err(
            "AskHuman detected an Agent caller but no trustworthy session id; refusing an unsafe project-wide fallback",
        ),
    }
}

/// 提问解析的入口包装：仅当出现 `--stdin` 时读取标准输入作为 Message，
/// 再交给纯函数 `args::parse_ask`（stdin 内容以参数注入，保持其无 IO 副作用）。
fn parse_ask_with_stdin(args: &[String], lang: Lang) -> Result<args::AskArgs, String> {
    let stdin_message = if args.iter().any(|a| a == "--stdin") {
        Some(read_stdin_message(lang))
    } else {
        None
    };
    args::parse_ask(args, lang, stdin_message)
}

/// 读取标准输入作为 Message 文本（`--stdin`）。
///
/// - stdin 为终端（无管道输入）时不阻塞等待，直接报错退出，避免挂起；
/// - 读取失败时报错退出；
/// - 去除结尾的一个换行（`\n` 或 `\r\n`，即 heredoc 末尾的固有换行），
///   其余（含前导/内部空白）原样保留。
fn read_stdin_message(lang: Lang) -> String {
    use std::io::{IsTerminal, Read};
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        eprintln!(
            "{}{}",
            i18n::err_prefix(lang),
            i18n::tr(lang, "cli.stdinIsTty")
        );
        exit(1);
    }
    let mut buf = String::new();
    if let Err(e) = stdin.read_to_string(&mut buf) {
        eprintln!("{}{}", i18n::err_prefix(lang), e);
        exit(1);
    }
    if let Some(stripped) = buf.strip_suffix('\n') {
        buf.truncate(stripped.len());
        if let Some(stripped) = buf.strip_suffix('\r') {
            buf.truncate(stripped.len());
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggestion(text: &str, recommended: bool) -> args::OptArg {
        args::OptArg {
            text: text.to_string(),
            recommended,
        }
    }

    fn todo(index: usize) -> crate::todos::TodoEntry {
        crate::todos::TodoEntry {
            id: format!("todo-{index}"),
            text: format!("todo {index}"),
            created_at_ms: index as u64,
            agent_kind: None,
            auto: false,
            attachments: Vec::new(),
        }
    }

    #[test]
    fn show_last_project_fallback_requires_a_non_agent_caller() {
        assert!(matches!(
            show_last_cli_scope(None, None, "/project".into()).unwrap(),
            crate::show_last::Scope::Project(project) if project == "/project"
        ));
        assert!(show_last_cli_scope(Some("codex".into()), None, "/project".into()).is_err());
        assert!(show_last_cli_scope(None, Some("session".into()), "/project".into()).is_err());
        assert!(matches!(
            show_last_cli_scope(
                Some("codex".into()),
                Some("session".into()),
                "/project".into()
            )
            .unwrap(),
            crate::show_last::Scope::AgentSession { agent_kind, session_id }
                if agent_kind == "codex" && session_id == "session"
        ));
    }

    #[test]
    fn caller_context_prefers_trusted_mcp_binding_and_ignores_stale_native_env() {
        let env = HashMap::from([
            (FROM_MCP_ENV.into(), "1".into()),
            (MCP_INSTANCE_ID_ENV.into(), "  instance  ".into()),
            (MCP_AGENT_KIND_ENV.into(), "cursor".into()),
            (MCP_AGENT_SESSION_ID_ENV.into(), " conversation ".into()),
            ("CODEX_THREAD_ID".into(), "stale-codex".into()),
        ]);
        assert_eq!(
            caller_context_from_env(&env),
            CallerContext {
                agent_kind: Some("cursor".into()),
                agent_session_id: Some("conversation".into()),
                mcp_instance_id: Some("instance".into()),
                from_mcp: true,
            }
        );

        for env in [
            HashMap::from([
                (FROM_MCP_ENV.into(), "1".into()),
                (MCP_AGENT_KIND_ENV.into(), "unknown".into()),
                (MCP_AGENT_SESSION_ID_ENV.into(), "session".into()),
            ]),
            HashMap::from([
                (FROM_MCP_ENV.into(), "true".into()),
                (MCP_AGENT_KIND_ENV.into(), "codex".into()),
            ]),
        ] {
            let context = caller_context_from_env(&env);
            assert!(context.from_mcp);
            assert!(context.agent_kind.is_none());
            assert!(context.agent_session_id.is_none());
        }
    }

    #[test]
    fn caller_context_detects_direct_agent_and_never_assigns_mcp_partition() {
        let env = HashMap::from([
            ("CURSOR_AGENT".into(), "1".into()),
            ("CURSOR_CONVERSATION_ID".into(), " conversation ".into()),
            (FROM_MCP_ENV.into(), "0".into()),
            (MCP_INSTANCE_ID_ENV.into(), "ignored".into()),
        ]);
        assert_eq!(
            caller_context_from_env(&env),
            CallerContext {
                agent_kind: Some("cursor".into()),
                agent_session_id: Some("conversation".into()),
                mcp_instance_id: None,
                from_mcp: false,
            }
        );
        assert_eq!(
            caller_context_from_env(&HashMap::new()),
            CallerContext::default()
        );
    }

    #[test]
    fn whats_next_orders_suggestions_todos_then_end_with_ten_total() {
        let question = whats_next_question_from_entries(
            &[
                suggestion("Write docs", false),
                suggestion("Add tests", true),
            ],
            (1..=10).map(todo).collect(),
            Lang::En,
        );

        assert_eq!(question.predefined_options.len(), WHATS_NEXT_MAX_OPTIONS);
        assert_eq!(question.predefined_options[0].text, "Write docs");
        assert!(!question.predefined_options[0].recommended);
        assert_eq!(question.predefined_options[1].text, "Add tests");
        assert!(question.predefined_options[1].recommended);
        assert_eq!(
            question.predefined_options[2].todo_id.as_deref(),
            Some("todo-1")
        );
        assert_eq!(
            question.predefined_options.last().unwrap().text,
            "End this turn"
        );
        assert!(question.message.contains('3'), "{}", question.message);
    }

    #[test]
    fn whats_next_silently_keeps_first_nine_suggestions() {
        let suggestions: Vec<_> = (1..=12)
            .map(|index| suggestion(&format!("suggestion {index}"), false))
            .collect();
        let question = whats_next_question_from_entries(&suggestions, vec![todo(1)], Lang::En);

        assert_eq!(question.predefined_options.len(), WHATS_NEXT_MAX_OPTIONS);
        assert_eq!(question.predefined_options[8].text, "suggestion 9");
        assert!(question.predefined_options[..9]
            .iter()
            .all(|option| option.todo_id.is_none()));
        assert_eq!(
            question.predefined_options.last().unwrap().text,
            "End this turn"
        );
        assert_eq!(question.message, "What should we do next?");
    }

    #[test]
    fn normalize_strips_whitespace_and_punctuation() {
        assert_eq!(
            normalize_whats_next_option_key("End this turn!"),
            "endthisturn"
        );
        assert_eq!(normalize_whats_next_option_key("  结束本轮。 "), "结束本轮");
        assert_eq!(normalize_whats_next_option_key("We're done"), "weredone");
        assert_eq!(
            normalize_whats_next_option_key("No more tasks"),
            "nomoretasks"
        );
    }

    #[test]
    fn spurious_end_detector_matches_table_and_builtin() {
        assert!(is_spurious_whats_next_end_option("End this turn"));
        assert!(is_spurious_whats_next_end_option("结束本轮"));
        assert!(is_spurious_whats_next_end_option("  结束本轮。"));
        assert!(is_spurious_whats_next_end_option("no more tasks!"));
        assert!(is_spurious_whats_next_end_option("Stop"));
        assert!(is_spurious_whats_next_end_option("先这样"));
        assert!(is_spurious_whats_next_end_option("We're done"));
        // Real tasks that only contain end-ish words stay.
        assert!(!is_spurious_whats_next_end_option("结束本轮的文档撰写"));
        assert!(!is_spurious_whats_next_end_option(
            "Stop the flaky e2e suite"
        ));
        assert!(!is_spurious_whats_next_end_option("Write docs"));
    }

    #[test]
    fn whats_next_drops_spurious_end_suggestions_and_keeps_one_builtin() {
        let question = whats_next_question_from_entries(
            &[
                suggestion("Write docs", false),
                suggestion("结束本轮", false),
                suggestion("End this turn", true),
                suggestion("no more work", false),
                suggestion("Add tests", true),
            ],
            (1..=10).map(todo).collect(),
            Lang::Zh,
        );

        let labels: Vec<&str> = question
            .predefined_options
            .iter()
            .map(|o| o.text.as_str())
            .collect();
        assert!(labels.contains(&"Write docs"));
        assert!(labels.contains(&"Add tests"));
        // Only the final built-in end option (Chinese UI); agent end-ish labels dropped.
        assert_eq!(labels.last().copied(), Some("结束本轮"));
        assert_eq!(
            labels.iter().filter(|t| **t == "结束本轮").count(),
            1,
            "{labels:?}"
        );
        assert!(
            labels[..labels.len() - 1]
                .iter()
                .all(|t| !is_spurious_whats_next_end_option(t)),
            "{labels:?}"
        );
        // Dropped end-ish suggestions free slots for more todos.
        assert!(
            question
                .predefined_options
                .iter()
                .filter(|o| o.todo_id.is_some())
                .count()
                >= 2
        );
    }

    #[test]
    fn whats_next_all_spurious_suggestions_leave_only_end_when_no_todos() {
        let question = whats_next_question_from_entries(
            &[
                suggestion("End this turn", false),
                suggestion("收工", false),
                suggestion("done", false),
            ],
            vec![],
            Lang::En,
        );
        assert_eq!(question.predefined_options.len(), 1);
        assert_eq!(question.predefined_options[0].text, "End this turn");
    }
}
