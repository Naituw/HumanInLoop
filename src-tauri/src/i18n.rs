//! 后端国际化：单一配置 `general.language`（auto|en|zh）解析为 `Lang`，
//! 用于 CLI / 窗口标题 / macOS 菜单·Dock / 通知 / 远程渠道等用户可见文案。
//!
//! 源语言为英文；缺失/未知一律回退英文。词条在各里程碑逐步扩充（M4/M5）。

use crate::config::AppConfig;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    /// 由配置语言字符串解析：显式 en/zh 直用；其余（auto/未知）跟随系统。
    pub fn resolve(cfg_language: &str) -> Lang {
        match cfg_language {
            "en" => Lang::En,
            "zh" => Lang::Zh,
            _ => Lang::from_system(),
        }
    }

    /// 跟随系统：首选语言以 "zh" 开头→中文，否则英文。
    pub fn from_system() -> Lang {
        match sys_locale::get_locale() {
            Some(l) if l.to_ascii_lowercase().starts_with("zh") => Lang::Zh,
            _ => Lang::En,
        }
    }

    /// 读取已保存配置解析当前界面语言。
    /// 仅需 `general.language`，故用 `load_without_secrets()`：语言探测绝不应触发钥匙串读取
    /// （否则 `--version`/`--help` 等无关命令也会读钥匙串，甚至在签名不匹配时弹密码框）。
    pub fn current() -> Lang {
        Lang::resolve(&AppConfig::load_without_secrets().general.language)
    }

    /// 解析后的语言码（"en" / "zh"）。供 CLI 上送 Daemon（A11：使 `auto` 跟随调用方）。
    pub fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Zh => "zh",
        }
    }
}

/// 二选一：按当前语言取英文或中文文案。
#[inline]
fn pick(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}

/// 错误行前缀（CLI/stderr）。
pub fn err_prefix(lang: Lang) -> &'static str {
    pick(lang, "Error: ", "错误: ")
}

/// 警告行前缀（CLI/stderr）。
pub fn warn_prefix(lang: Lang) -> &'static str {
    pick(lang, "Warning: ", "警告: ")
}

/// 来源头部文案（「Question from {source}」/「Message from {source}」）。
///
/// 默认来源 "the Loop" 是 human-in-the-loop 的固定英文短语，一律保持英文；自定义来源
/// （用户经 `ASKHUMAN_ENV_SOURCE_NAME` 指定）则按界面语言本地化。
/// `key` 取 `"channel.questionFrom"` / `"channel.messageFrom"`。
pub fn source_header(lang: Lang, key: &'static str, source: &str) -> String {
    let effective = if source == crate::models::DEFAULT_SOURCE_NAME {
        Lang::En
    } else {
        lang
    };
    tr(effective, key).replace("{source}", source)
}

/// 词条查询：源语言英文，缺失 key 原样返回（兜底）。
/// 含 `{x}` 占位符的模板由调用方用 `str::replace` 填充。
/// `key` 取 `'static`（调用方均传字面量），以便兜底分支可原样返回。
pub fn tr(lang: Lang, key: &'static str) -> &'static str {
    match key {
        // 结果区块字段标记现为 output.rs 的固定英文常量（不本地化，见 `cli::output::MARKER_*`）。

        // —— 结果状态文案 ——
        "status.cancel" => pick(
            lang,
            "The user canceled. You must ask again whether they really want to cancel, and keep asking until they give a clear answer.",
            "用户取消了操作，你必须重新询问用户是否确定要取消，直到用户给出明确答复",
        ),
        "status.unanswered" => pick(lang, "The user did not answer this question", "用户未回答此问题"),
        // 接管 Claude AskUserQuestion 时，单选题的题面标注（我们的卡片按整卡多选渲染，spec D5）。
        "askQuestion.pickOne" => pick(lang, "(pick one)", "（只选一项）"),
        // 重放（spec duplicate-ask-coalescing D7）：同一个提问刚被回答过，这次没有再打扰人。
        "status.replayed" => pick(
            lang,
            "This is the answer the user gave {n}s ago to an identical question. They were not asked again.",
            "这是用户在 {n} 秒前对完全相同问题给出的回答，本次没有再打扰用户",
        ),
        "status.confirmContinue" => pick(lang, "User confirmed to continue", "用户确认继续"),

        // —— 窗口标题 ——
        "title.popup" => "AskHuman",
        "title.settings" => pick(lang, "AskHuman Settings", "AskHuman 设置"),
        "title.history" => pick(lang, "AskHuman History", "AskHuman 历史记录"),
        "title.agents" => pick(lang, "AskHuman Agents", "AskHuman Agent 状态"),
        "title.interject" => pick(lang, "Message to Agent", "给 Agent 发消息"),
        "title.todos" => pick(lang, "AskHuman Todos", "AskHuman 待办"),
        "title.newTask" => pick(lang, "New Agent Task", "新建 Agent 任务"),
        "title.forkTask" => pick(lang, "Fork Agent Session", "Fork Agent 会话"),

        // —— macOS 附件右键菜单 ——
        "menu.open" => pick(lang, "Open", "打开"),
        "menu.openWith" => pick(lang, "Open With", "打开方式"),
        "menu.other" => pick(lang, "Other…", "其他…"),
        "menu.quickLook" => pick(lang, "Quick Look “{name}”", "快速查看「{name}」"),
        "menu.revealInFinder" => pick(lang, "Show in Finder", "在访达中显示"),
        "menu.copyFile" => pick(lang, "Copy “{name}”", "拷贝「{name}」"),
        "menu.copyPath" => pick(lang, "Copy Path", "拷贝路径"),
        "menu.appFallback" => pick(lang, "App", "应用"),

        // —— CLI 解析/分发错误 ——
        "cli.missingContent" => pick(lang, "missing question content", "缺少提问内容"),
        "cli.seeAgentHelp" => pick(
            lang,
            "Run `{prog} --agent-help` to see the full asking usage.",
            "运行 `{prog} --agent-help` 查看完整的提问用法说明。",
        ),
        "cli.unknownOption" => pick(lang, "unknown option {opt}", "未知选项 {opt}"),
        "cli.unknownOptionColon" => pick(lang, "unknown option: {opt}", "未知选项: {opt}"),
        "cli.optionMissingValue" => pick(lang, "{opt} option is missing a value", "{opt} 选项缺少参数值"),
        "cli.optionBeforeQuestion" => pick(
            lang,
            "{opt} cannot appear before the first question (-q)",
            "{opt} 不能出现在第一个问题(-q)之前",
        ),
        "cli.positionalOnlyMessage" => pick(
            lang,
            "unexpected argument '{arg}': only the Message may be positional, and it must precede all options. If a value contains spaces or special characters, pass it as a single shell argument, quoting or escaping it as required.",
            "意外的参数 '{arg}'：仅 Message 可作为位置参数，且须位于所有选项之前。若某个值含空格或特殊字符，请将其作为单个 shell 参数传入，并按需正确引用或转义。",
        ),
        "cli.stdinWithPositional" => pick(
            lang,
            "--stdin cannot be combined with a positional <Message>",
            "--stdin 不能与位置参数 <Message> 同时使用",
        ),
        "cli.stdinIsTty" => pick(
            lang,
            "--stdin was given but stdin is a terminal (no piped input)",
            "指定了 --stdin，但 stdin 是终端（没有管道输入）",
        ),
        "cli.unsupportedOutputFormat" => pick(
            lang,
            "unsupported output format: {value} (use text or json)",
            "不支持的输出格式: {value}（可用 text 或 json）",
        ),
        "cli.selectOnlyNeedsOptions" => pick(
            lang,
            "--select-only requires every question to have options (-o)",
            "--select-only 要求每个问题都有选项(-o)",
        ),
        "cli.whatsNextConflict" => pick(
            lang,
            "--whats-next cannot be combined with {opt}",
            "--whats-next 不能与 {opt} 同时使用",
        ),

        // —— whats-next 固定提问（spec todo-whats-next D2）——
        "whatsNext.question" => pick(lang, "What should we do next?", "接下来做什么？"),
        "whatsNext.endOption" => pick(lang, "End this turn", "结束本轮"),
        // 待办选项的展示前缀（whats-next / Stop 卡共用）；发给 agent 的任务文本会剥掉它
        // （`output::strip_todo_prefix`，两种语言都尝试）。
        "whatsNext.todoPrefix" => pick(lang, "Run todo: ", "执行待办："),
        "todo.attachmentBadge" => pick(lang, "【{n} attachments】", "【{n} 个附件】"),
        // 选项类展示点超过 MAX_OPTION_TODOS 时的溢出提示（附正文尾部，第 14 轮定案）。
        "todo.moreNote" => pick(
            lang,
            "…{n} more todo(s) not listed (view or reorder in the Todos window or /todo).",
            "……还有 {n} 条待办未列出（可在待办窗口或 /todo 查看调序）。",
        ),

        // —— CLI todo 子命令（spec todo-whats-next D6）——
        "todo.added" => pick(lang, "Added todo #{n}: {text}", "已添加待办 #{n}: {text}"),
        "todo.addedAuto" => pick(
            lang,
            "Added auto-run todo #{n}: {text}",
            "已添加自动执行待办 #{n}: {text}",
        ),
        // 列表 / 卡片中自动执行待办的标记（第 17 轮定案）。
        "todo.autoMark" => pick(lang, "⚡auto", "⚡自动"),
        "todo.empty" => pick(lang, "No pending todos for this project", "本项目暂无待办"),
        "todo.listHeader" => pick(lang, "Pending todos ({project}):", "待办列表（{project}）:"),
        "todo.removed" => pick(lang, "Removed todo #{n}: {text}", "已删除待办 #{n}: {text}"),
        "todo.invalidIndex" => pick(
            lang,
            "invalid todo number: {n} (run `{prog} todo list` to see numbers)",
            "无效的待办编号: {n}（运行 `{prog} todo list` 查看编号）",
        ),
        "todo.cleared" => pick(lang, "Cleared {n} todo(s)", "已清空 {n} 条待办"),
        "todo.clearConfirm" => pick(
            lang,
            "Clear all {n} todo(s) of this project? [y/N] ",
            "确认清空本项目全部 {n} 条待办？[y/N] ",
        ),
        "todo.clearAborted" => pick(lang, "Aborted; nothing was cleared", "已取消，未清空"),
        "todo.missingText" => pick(lang, "todo add is missing the todo text", "todo add 缺少待办内容"),
        "todo.persistFailed" => pick(
            lang,
            "failed to save todo (could not write ~/.askhuman/state/todos.json — check permissions or agent sandbox write access)",
            "保存待办失败（无法写入 ~/.askhuman/state/todos.json — 请检查权限或 agent 沙箱写权限）",
        ),
        "todo.unknownSubcommand" => pick(
            lang,
            "unknown todo subcommand: {cmd} (use add / list / attach / detach / rm / clear)",
            "未知的 todo 子命令: {cmd}（可用 add / list / attach / detach / rm / clear）",
        ),

        // —— 文件附件解析错误 ——
        "cli.fileNotFound" => pick(lang, "file not found or inaccessible: {path}", "文件不存在或无法访问: {path}"),
        "cli.notAFile" => pick(lang, "not a file: {path}", "不是文件: {path}"),

        // —— 图片落盘错误 ——
        "cli.createImageDirFailed" => pick(lang, "failed to create image directory: {path}", "创建图片目录失败: {path}"),
        "cli.writeImageFailed" => pick(lang, "failed to write image: {path}", "写入图片失败: {path}"),
        "cli.imageDecodeFailed" => pick(lang, "failed to decode image base64", "图片 base64 解码失败"),

        // —— 运行/渠道可用性（stderr，含自带前缀的整行）——
        "app.popupUnavailableFellBack" => pick(
            lang,
            "Local popup unavailable: {reason}; using messaging channel instead",
            "本地弹窗不可用：{reason}；已改用消息渠道",
        ),
        "app.popupUnavailableNoChannel" => pick(
            lang,
            "Local popup unavailable: {reason}, and no messaging channel is configured",
            "本地弹窗不可用：{reason}，且未配置可用的消息渠道",
        ),
        "app.popupDisabledNoChannel" => pick(
            lang,
            "Local popup is disabled, and no messaging channel is configured",
            "本地弹窗已禁用，且未配置可用的消息渠道",
        ),
        "app.noChannel" => pick(lang, "no available channel — {reason}", "无可用的通信 Channel — {reason}"),
        "app.popupStartFailedFellBack" => pick(
            lang,
            "Local popup failed to start: {e}; using messaging channel instead",
            "本地弹窗启动失败：{e}；已改用消息渠道",
        ),
        "app.popupStartFailedNoChannel" => pick(
            lang,
            "Local popup failed to start: {e}, and no messaging channel is configured",
            "本地弹窗启动失败：{e}，且未配置可用的消息渠道",
        ),
        "app.runtimeCreateFailed" => pick(lang, "failed to create runtime: {e}", "无法创建运行时: {e}"),
        "app.telegramInvalid" => pick(lang, "invalid Telegram config: {e}", "Telegram 配置无效: {e}"),
        "app.dingtalkInvalid" => pick(lang, "invalid DingTalk config: {e}", "钉钉配置无效: {e}"),
        "app.feishuInvalid" => pick(lang, "invalid Feishu config: {e}", "飞书配置无效: {e}"),
        "app.slackInvalid" => pick(lang, "invalid Slack config: {e}", "Slack 配置无效: {e}"),
        "app.sessionEndedNoResult" => pick(
            lang,
            "messaging session ended without a result",
            "消息渠道会话结束但未获得结果",
        ),
        "app.settingsLaunchFailed" => pick(lang, "failed to launch settings: {e}", "无法启动设置界面: {e}"),
        "app.historyLaunchFailed" => pick(lang, "failed to launch history window: {e}", "无法启动历史窗口: {e}"),
        "app.todosLaunchFailed" => pick(lang, "failed to launch todos window: {e}", "无法启动待办窗口: {e}"),
        "app.agentsLaunchFailed" => pick(lang, "failed to launch agents window: {e}", "无法启动 Agent 状态窗口: {e}"),
        "app.noDisplay" => pick(
            lang,
            "no graphical display (neither DISPLAY nor WAYLAND_DISPLAY is set)",
            "无图形显示环境（DISPLAY / WAYLAND_DISPLAY 均未设置）",
        ),
        "app.noWebkitgtk" => pick(
            lang,
            "missing WebKitGTK on the system (e.g. libwebkit2gtk-4.1)",
            "系统缺少 WebKitGTK（如 libwebkit2gtk-4.1）",
        ),

        // —— 远程渠道（Telegram / 钉钉）发给用户的文案 ——
        // 来源头部：默认来源 "the Loop"（human-in-the-loop 固定短语）经 source_header() 强制英文；
        // 自定义来源按界面语言本地化（故此处中文照常给出译文）。
        "channel.questionFrom" => pick(lang, "Question from {source}", "来自 {source} 的提问"),
        "channel.messageFrom" => pick(lang, "Message from {source}", "来自 {source} 的消息"),
        "channel.messageTitle" => pick(lang, "Message", "消息"),
        "channel.questionTitle" => pick(lang, "Question", "提问"),
        "channel.questionIndexed" => pick(lang, "Question {i}/{n}", "问题 {i}/{n}"),
        // 推荐选项的显示文本前缀（尾随空格即与原文的分隔；提交值不含前缀）。
        "channel.recommendedPrefix" => pick(lang, "[👍Recommended] ", "【👍推荐】 "),
        // Slack 原生选项 description 推荐文案（控件内展示，无括号）。
        "channel.slackRecommended" => pick(lang, "👍 Recommended", "👍 推荐"),
        // 飞书选项 checker 的推荐前缀：lark_md 绿色含括号（checker text 用 lark_md）。
        "channel.feishuRecommendedPrefix" => pick(
            lang,
            "<font color='green'>[👍Recommended]</font> ",
            "<font color='green'>【👍推荐】</font> ",
        ),
        // Todo markers replace the legacy text prefix only in rich-text-capable channels.
        "channel.feishuTodoPrefix" => pick(
            lang,
            "<font color='orange'>【TODO】</font> ",
            "<font color='orange'>【TODO】</font> ",
        ),
        // 钉钉选项 md 的推荐徽标文案（card.rs 用 h5 字号 + 绿色 font 包裹，含括号）。
        "channel.dingtalkRecommended" => pick(lang, "[👍Recommended]", "【👍推荐】"),
        "channel.dingtalkTodo" => pick(lang, "【TODO】", "【TODO】"),
        "channel.tgSendButton" => pick(lang, "↑ Submit", "↑ 提交"),
        // 抢答收尾：赢家端名称 + 卡片终态状态行。
        "channel.sourcePopup" => pick(lang, "Popup", "弹窗"),
        "channel.sourceTelegram" => pick(lang, "Telegram", "Telegram"),
        "channel.sourceDingTalk" => pick(lang, "DingTalk", "钉钉"),
        "channel.sourceFeishu" => pick(lang, "Feishu", "飞书"),
        "channel.sourceSlack" => pick(lang, "Slack", "Slack"),
        // Cancel source: the caller (CLI/terminal cancelled the request).
        "channel.sourceCaller" => pick(lang, "Caller", "调用方"),
        "channel.tgReplied" => pick(lang, "✅ Replied", "✅ 已回复"),
        "channel.tgAnsweredVia" => pick(lang, "✅ Answered via {source}", "✅ 已在{source}回答"),
        // Telegram cancelled terminal state (uses an emoji prefix like other tg states).
        "channel.tgCancelled" => pick(lang, "🚫 Cancelled", "🚫 已取消"),
        "channel.tgCancelledBy" => pick(lang, "🚫 Cancelled by {source}", "🚫 已被{source}取消"),
        // 钉钉卡片终态文案（绑定模板变量 submit_status）。卡片自带样式，文案不加 emoji 前缀。
        "channel.ddSubmitted" => pick(lang, "Submitted", "已提交"),
        "channel.ddAnsweredVia" => pick(lang, "Answered via {source}", "已在{source}回答"),
        // Cancelled terminal state (card carries its own style, no emoji prefix).
        "channel.ddCancelled" => pick(lang, "Cancelled", "已取消"),
        "channel.ddCancelledBy" => pick(lang, "Cancelled by {source}", "已被{source}取消"),
        "channel.tgActionHint" => pick(
            lang,
            "💬 To add more, just send text messages here. Anything you send after this card (before tapping Submit) will be included.",
            "💬 需要补充可直接在这里发文字消息。本卡片发出后、点「提交」前你发送的文字都会一并提交。",
        ),
        // Strict (select-only) hints: free text is ignored, only the buttons count.
        "channel.tgActionHintSelectSingle" => pick(
            lang,
            "👇 Pick one option, then tap Submit.",
            "👇 请选择一个选项后点「提交」。",
        ),
        "channel.tgActionHintSelectMulti" => pick(
            lang,
            "👇 Pick one or more options, then tap Submit.",
            "👇 请选择一个或多个选项后点「提交」。",
        ),
        "channel.tgSelectRequired" => pick(
            lang,
            "Please pick an option first.",
            "请先选择一个选项。",
        ),
        "channel.fileSendFailed" => pick(lang, "⚠️ Failed to send file: {name}", "⚠️ 文件发送失败：{name}"),
        // Telegram question title (fallback when there's no source header), consistent with DingTalk/Feishu.
        "channel.tgTitleFallback" => pick(lang, "Question", "提问"),
        "channel.ddTitleFallback" => pick(lang, "Question", "提问"),
        "channel.ddHintFree" => pick(
            lang,
            "👉 Just reply with text; you can also send images / files",
            "👉 直接回复文字即可；也可发送图片 / 文件",
        ),
        "channel.ddHintOptions" => pick(
            lang,
            "👉 Reply with the option number(s) (comma-separated for multiple, e.g. 1,3), or just type your reply; you can also send images / files",
            "👉 回复编号选择（多选用逗号，如 1,3），或直接输入文字；也可发送图片 / 文件",
        ),
        "channel.cardSendButton" => pick(lang, "Send", "发送"),

        // —— 渠道本地诊断（stderr，含 warn/err 前缀由调用方拼接）——
        "channel.tgConfigInvalidSkip" => pick(
            lang,
            "invalid Telegram config, skipping this channel: {e}",
            "Telegram 配置无效，已跳过该 Channel: {e}",
        ),
        "channel.ddConfigInvalidSkip" => pick(
            lang,
            "invalid DingTalk config, skipping this channel: {e}",
            "钉钉配置无效，已跳过该 Channel: {e}",
        ),
        "channel.fileSendFailedLog" => pick(lang, "failed to send file: {path}: {e}", "文件发送失败: {path}: {e}"),
        "channel.ddMessageSendFailed" => pick(lang, "failed to send DingTalk Message: {e}", "钉钉 Message 发送失败: {e}"),
        "channel.ddFileSendFailedLog" => pick(lang, "failed to send DingTalk file: {path}: {e}", "钉钉文件发送失败: {path}: {e}"),
        "channel.ddQuestionSendFailed" => pick(lang, "failed to send DingTalk question: {e}", "钉钉提问发送失败: {e}"),
        "channel.ddCardDeliverFailed" => pick(lang, "failed to deliver DingTalk card, falling back to text: {e}", "钉钉互动卡片投放失败，回退纯文本: {e}"),
        "channel.ddImageDownloadFailed" => pick(lang, "failed to download DingTalk image: {e}", "钉钉图片下载失败: {e}"),
        "channel.ddFileDownloadFailed" => pick(lang, "failed to download DingTalk file: {e}", "钉钉文件下载失败: {e}"),

        // —— 飞书渠道：发给用户的文案 + 本地诊断 ——
        // 卡片终态文案（PATCH 卡片 / toast）。卡片自带样式，文案不加 emoji 前缀。
        "channel.fsSubmitted" => pick(lang, "Submitted", "已提交"),
        "channel.fsAnsweredVia" => pick(lang, "Answered via {source}", "已在{source}回答"),
        // Cancelled terminal state (card carries its own style, no emoji prefix).
        "channel.fsCancelled" => pick(lang, "Cancelled", "已取消"),
        "channel.fsCancelledBy" => pick(lang, "Cancelled by {source}", "已被{source}取消"),
        "channel.fsTitleFallback" => pick(lang, "Question", "提问"),
        // 卡片表单：输入框占位 + 提交按钮文案。
        "channel.fsInputPlaceholder" => pick(lang, "Add a note (optional)", "补充说明（可选）"),
        // 终态卡上补充文字的小标题（引用块之上）。
        "channel.fsNoteLabel" => pick(lang, "My note:", "我的补充："),
        "channel.fsSubmitButton" => pick(lang, "Submit", "提交"),
        "channel.fsConfigInvalidSkip" => pick(
            lang,
            "invalid Feishu config, skipping this channel: {e}",
            "飞书配置无效，已跳过该 Channel: {e}",
        ),
        "channel.fsMessageSendFailed" => pick(lang, "failed to send Feishu Message: {e}", "飞书 Message 发送失败: {e}"),
        "channel.fsFileSendFailedLog" => pick(lang, "failed to send Feishu file: {path}: {e}", "飞书文件发送失败: {path}: {e}"),
        "channel.fsQuestionSendFailed" => pick(lang, "failed to send Feishu question: {e}", "飞书提问发送失败: {e}"),
        "channel.fsCardDeliverFailed" => pick(lang, "failed to deliver Feishu card, falling back to text: {e}", "飞书互动卡片投放失败，回退纯文本: {e}"),
        "channel.fsImageDownloadFailed" => pick(lang, "failed to download Feishu image: {e}", "飞书图片下载失败: {e}"),
        "channel.fsFileDownloadFailed" => pick(lang, "failed to download Feishu file: {e}", "飞书文件下载失败: {e}"),

        // —— IM 会话期自动激活：入站回执 / 命令 / 状态文本 ——
        // 激活确认（发 here、或普通消息触发切换时回执；用语中性，无论是否真的切换都贴切）。
        "autoChannel.activated" => pick(
            lang,
            "Questions will now be sent to this channel.",
            "后续提问将发送到此渠道。",
        ),
        // 补推在途后缀（仅当补推了 N(>0) 条在途未答问题时追加到激活回执后）。
        "autoChannel.pending" => pick(lang, " ({n} pending question(s) delivered)", "（已补推 {n} 条待答问题）"),
        // 反激活提示（活跃槽切到别处时发给旧渠道；{target} = 新渠道展示名，如「弹窗」「钉钉」）。
        "autoChannel.deactivated" => pick(
            lang,
            "Questions have moved to {target} and will no longer be sent here. Send any message to switch back.",
            "后续提问已切换到「{target}」，将不在此发送。发送任何消息即可切回此渠道。",
        ),
        // /status 文本分组标题。
        "autoChannel.statusWorking" => pick(lang, "Working", "工作中"),
        "autoChannel.statusIdle" => pick(lang, "Idle", "空闲"),
        // /status 单行占位（标题 / 项目缺失时）。
        "autoChannel.noTitle" => pick(lang, "(untitled)", "（未命名）"),
        "autoChannel.noProject" => pick(lang, "unknown project", "未知项目"),
        // /status empty state points to lifecycle inside the Agent integration card.
        "autoChannel.statusEmpty" => pick(
            lang,
            "No working or idle agents right now.\n(Agent status relies on Lifecycle Tracking; check the relevant Agent card under Settings → Agents.)",
            "当前没有工作中或空闲的 agent。\n（Agent 状态依赖生命周期追踪；请在 设置 → Agents 的对应 Agent 卡中检查。）",
        ),
        // /status <编号> 详情：未找到该编号。`{p}` 为渠道命令前缀（Slack 用 `!`，其余 `/`）。
        "autoChannel.statusDetailNotFound" => pick(
            lang,
            "No agent numbered {id}. Send {p}status to see the list.",
            "没有编号 {id} 的 agent。发送 {p}status 查看列表。",
        ),
        // /status <编号> 详情：解析不到当前活动。
        "autoChannel.statusNoActivity" => pick(lang, "(no activity yet)", "（暂无可解析的活动）"),
        "autoChannel.activityHeading" => pick(lang, "Latest activity", "最近动态"),
        // /status <编号> 详情头部的状态词（单行用，区别于分组标题）。
        "autoChannel.stateWorking" => pick(lang, "working", "工作中"),
        "autoChannel.stateIdle" => pick(lang, "idle", "空闲"),
        "autoChannel.stateEnded" => pick(lang, "ended", "已结束"),
        // 当前活动里的工具类别词（仅归一化常见工具：读/写/运行命令）。
        "autoChannel.activityRun" => pick(lang, "Run", "运行命令"),
        "autoChannel.activityRead" => pick(lang, "Read", "读取文件"),
        "autoChannel.activityWrite" => pick(lang, "Edit", "写入文件"),

        // —— /msg 插话（spec agent-interject D9）。编号复用 /status 的稳定 seq。 ——
        // 用法提示（编号缺省 / 非数字时）。
        "autoChannel.msgUsage" => pick(
            lang,
            "Usage: {p}msg <n> <text> — queue a message for agent n (delivered at its next tool call); {p}msg <n> — show pending; {p}msg-clear <n> — revoke.",
            "用法：{p}msg <编号> <内容> — 给该 agent 排队一条插话（其下一次工具调用时送达）；{p}msg <编号> — 查看待送达；{p}msg-clear <编号> — 撤回。",
        ),
        // 追加成功回执（{n} = 当前待送达条数）。
        "autoChannel.msgQueued" => pick(
            lang,
            "Queued — {n} message(s) pending delivery to this agent.",
            "已排队，该 agent 共 {n} 条待送达。",
        ),
        // 追加时恰有 hook 挂起等待 → 立即送达（待送达清零）。
        "autoChannel.msgDeliveredNow" => pick(
            lang,
            "Delivered — the agent was waiting and received it immediately.",
            "已送达 — 该 agent 正在等待，消息已立即转交。",
        ),
        // /msg <编号> 回显头（{n} = 条数；正文换行接在后面）。
        "autoChannel.msgEchoHeader" => pick(
            lang,
            "Pending for this agent ({n}):",
            "该 agent 待送达插话（{n} 条）：",
        ),
        // 无待送达（回显 / 撤回时）。
        "autoChannel.msgNone" => pick(
            lang,
            "No pending message for this agent.",
            "该 agent 暂无待送达插话。",
        ),
        // 撤回成功。
        "autoChannel.msgCleared" => pick(
            lang,
            "Pending message revoked.",
            "已撤回待送达插话。",
        ),
        // 排队插话被 agent 真正消费（下一次工具调用）后回推来源渠道的「已阅读」回执（{id}=编号）。
        "autoChannel.msgReadReceipt" => pick(
            lang,
            "✅ Your message to [{id}] was read by the agent.",
            "✅ 你发给 [{id}] 的插话已被 Agent 阅读。",
        ),
        // —— `/msg` 一次性消息输入卡（spec im-msg-compose-card）。——
        "msgCard.title" => pick(
            lang,
            "Message [{id}] {agent}",
            "给 [{id}] {agent} 发送消息",
        ),
        "msgCard.recoveredTitle" => pick(lang, "Send message", "发送消息"),
        "msgCard.target" => pick(lang, "Target", "目标"),
        "msgCard.pendingNone" => pick(lang, "No message is pending.", "暂无待送达消息。"),
        "msgCard.pendingCount" => pick(
            lang,
            "{n} message(s) pending:",
            "待送达 {n} 条：",
        ),
        "msgCard.previewOmitted" => pick(
            lang,
            "… {n} character(s) not shown …",
            "… 还有 {n} 字未显示 …",
        ),
        "msgCard.inputLabel" => pick(lang, "Message", "消息"),
        "msgCard.inputPlaceholder" => pick(
            lang,
            "Enter the message to send to the agent (up to 3000 characters)",
            "输入要发送给 Agent 的消息（最多 3000 字）",
        ),
        "msgCard.send" => pick(lang, "Send", "发送"),
        "msgCard.queuedShort" => pick(lang, "Queued", "已排队"),
        "msgCard.inputRequired" => pick(lang, "Enter a message before sending.", "请输入消息后再发送。"),
        "msgCard.inputTooLong" => pick(
            lang,
            "The message is too long (maximum {n} characters).",
            "消息过长（最多 {n} 字）。",
        ),
        "msgCard.expired" => pick(
            lang,
            "This message card has expired; nothing was sent. Send {p}msg again.",
            "消息卡已过期，未发送。请重新发送 {p}msg。",
        ),
        "msgCard.openFailed" => pick(
            lang,
            "Could not open the message card. Use {p}msg {id} <text> instead.",
            "无法打开消息卡，请改用 {p}msg {id} <内容>。",
        ),
        // grok 不支持插话（无可靠传话通道，spec agent-interject D1）。
        "autoChannel.msgGrokUnsupported" => pick(
            lang,
            "This agent (Grok) does not support interjection.",
            "该 agent（Grok）不支持插话。",
        ),
        // 会话已结束：无处送达。
        "autoChannel.msgEnded" => pick(
            lang,
            "This agent session has ended; nothing to deliver to.",
            "该 agent 会话已结束，无法插话。",
        ),

        // —— Dynamic guidance / /help (spec im-help-rich-text). ——
        // Content strings are markup-free. Channel renderers own lists, code, emphasis, and color.
        "autoChannel.helpTitle" => pick(
            lang,
            "AskHuman is running",
            "AskHuman 正在运行",
        ),
        "autoChannel.helpIntro" => pick(
            lang,
            "Use a command below, or send the phrase at the end of an item.",
            "可使用下列命令，或直接发送每项末尾的短语。",
        ),
        "autoChannel.helpGroupAgent" => pick(lang, "Agent management", "Agent 管理"),
        "autoChannel.helpGroupCode" => pick(lang, "Code and records", "代码与记录"),
        "autoChannel.helpGroupTodo" => pick(lang, "Project todos", "项目待办"),
        "autoChannel.helpGroupChannel" => pick(lang, "Channel and help", "渠道与帮助"),
        "autoChannel.helpDescStatus" => pick(
            lang,
            "List agents; add a number to view current activity",
            "列出 Agent；指定编号查看当前活动",
        ),
        "autoChannel.helpDescNew" => pick(
            lang,
            "Create a new Agent task on your computer",
            "在电脑上创建新的 Agent 任务",
        ),
        "autoChannel.helpDescFork" => pick(
            lang,
            "Fork a working or idle Agent session",
            "从工作中或空闲的 Agent 会话分叉",
        ),
        "autoChannel.helpPhraseFork" => pick(lang, "fork", "分叉会话"),
        "autoChannel.helpDescWatch" => pick(
            lang,
            "Follow an agent with a live status card",
            "关注 Agent 的实时状态",
        ),
        "autoChannel.helpDescUnwatch" => pick(
            lang,
            "Stop following one or all agents",
            "取消一个或全部关注",
        ),
        "autoChannel.helpDescMsg" => pick(
            lang,
            "Queue a message for the agent's next tool call",
            "排队插话，下次工具调用时送达",
        ),
        "autoChannel.helpDescMsgClear" => pick(
            lang,
            "Discard the agent's queued message",
            "撤回该 Agent 待送达的插话",
        ),
        "autoChannel.helpDescYolo" => pick(
            lang,
            "View or turn off Codex YOLO sessions",
            "查看或关闭 Codex YOLO 会话",
        ),
        "autoChannel.helpDescDiff" => pick(
            lang,
            "Export unstaged changes as an attachment",
            "导出未暂存变更（附件）",
        ),
        "autoChannel.helpDescStage" => pick(
            lang,
            "Confirm, then stage unstaged changes",
            "确认后暂存未暂存改动",
        ),
        "autoChannel.helpDescTranscript" => pick(
            lang,
            "Export the full session transcript",
            "导出完整会话记录（附件）",
        ),
        "autoChannel.helpDescTodo" => pick(
            lang,
            "Choose a project to view or add todos",
            "选择项目查看待办或新增一条",
        ),
        "autoChannel.helpDescTodoRm" => pick(
            lang,
            "Choose a project and delete todos",
            "选择项目并删除待办",
        ),
        "autoChannel.helpDescTodoAuto" => pick(
            lang,
            "Toggle or add auto-run todos",
            "切换或新增自动执行待办",
        ),
        "autoChannel.helpDescHere" => pick(
            lang,
            "Route future questions to this channel",
            "把后续提问切到此渠道",
        ),
        "autoChannel.helpDescHelp" => pick(lang, "Show this help", "显示这份帮助"),
        "autoChannel.helpPhraseStatus" => pick(lang, "status", "状态"),
        "autoChannel.helpPhraseNew" => pick(lang, "new", "新任务"),
        "autoChannel.helpPhraseWatch" => pick(lang, "watch", "关注"),
        "autoChannel.helpPhraseUnwatch" => pick(lang, "unwatch", "取消关注"),
        "autoChannel.helpPhraseMsg" => pick(lang, "message", "插话"),
        "autoChannel.helpPhraseYolo" => pick(lang, "yolo", "YOLO"),
        "autoChannel.helpPhraseDiff" => pick(lang, "diff", "查看变更"),
        "autoChannel.helpPhraseStage" => pick(lang, "stage", "暂存"),
        "autoChannel.helpPhraseTranscript" => pick(lang, "transcript", "导出会话"),
        "autoChannel.helpPhraseTodo" => pick(lang, "todo", "待办"),
        "autoChannel.helpPhraseTodoRm" => pick(lang, "delete todo", "删待办"),
        "autoChannel.helpPhraseTodoAuto" => pick(lang, "auto todo", "自动待办"),
        "autoChannel.helpPhraseHere" => pick(lang, "here", "这里"),
        "autoChannel.helpPhraseHelp" => pick(lang, "help", "帮助"),
        "autoChannel.helpPhraseHint" => pick(
            lang,
            "Say 「{phrase}」",
            "直接说「{phrase}」",
        ),
        // 有在途提问时的作答指引。
        "autoChannel.helpAnswering" => pick(
            lang,
            "There is a question waiting: choose options or type in the card, then tap Submit. You can also send images/files to attach them to your answer.",
            "当前有待回答的提问：在卡片中选择或输入后点「提交」；也可直接发送图片/文件补充到你的回答。",
        ),
        // 无在途提问时。
        "autoChannel.helpNoQuestion" => pick(lang, "No question is in progress right now.", "当前暂无进行中的提问。"),
        // 自动激活开时的切槽提示。
        "autoChannel.helpSwitchHint" => pick(
            lang,
            "Tip: send any text to route questions to this channel.",
            "提示：发送任意文字即可把提问切到此渠道接收。",
        ),

        // —— 作答内容被接受的即时确认（spec R2）：仅内容确实被接受进答案时由渠道会话发送。 ——
        "autoChannel.ackImageCard" => pick(
            lang,
            "✅ Got it — this image will be attached to your answer. Tap Submit on the card to finish.",
            "✅ 已收到，将把该图片加入你的回答；请在卡片点「提交」完成。",
        ),
        "autoChannel.ackFileCard" => pick(
            lang,
            "✅ Got it — this file will be attached to your answer. Tap Submit on the card to finish.",
            "✅ 已收到，将把该文件加入你的回答；请在卡片点「提交」完成。",
        ),
        "autoChannel.ackTextCard" => pick(
            lang,
            "✅ Got it — this text will be added to your answer. Tap Submit on the card to finish.",
            "✅ 已收到，将把该内容加入你的回答；请在卡片点「提交」完成。",
        ),
        "autoChannel.ackImageFallback" => pick(
            lang,
            "✅ Got it — this image will be used as your answer.",
            "✅ 已收到，将把该图片作为你的回答。",
        ),
        "autoChannel.ackFileFallback" => pick(
            lang,
            "✅ Got it — this file will be used as your answer.",
            "✅ 已收到，将把该文件作为你的回答。",
        ),
        "autoChannel.ackTextFallback" => pick(
            lang,
            "✅ Got it — recorded as your answer.",
            "✅ 已收到，将作为你的回答。",
        ),

        // —— 自动识别 ID 成功回执（spec R5）：只报字段名、不回显 ID。{field} 由调用方填本地化字段名。 ——
        "autoChannel.detectAck" => pick(
            lang,
            "✅ Detected — {field} has been filled in automatically.",
            "✅ 识别成功，已自动填入{field}。",
        ),
        "autoChannel.detectFieldUserId" => pick(lang, "User ID", "用户 ID"),
        "autoChannel.detectFieldOpenId" => pick(lang, "OpenID", "用户 OpenID"),

        // —— /watch 实时关注（spec docs/specs/im-watch.md；四渠道全支持）——
        // 渠道门控：理论上已无渠道触发（四渠道全支持），保留兜底未来新渠道。
        "watch.unsupported" => pick(
            lang,
            "Live watch is available on Feishu, Telegram, Slack and DingTalk.",
            "「实时关注」支持飞书、Telegram、Slack、钉钉渠道。",
        ),
        // 关注上限。`{p}` 为渠道命令前缀（Slack 用 `!`，其余 `/`）。
        "watch.limit" => pick(
            lang,
            "Watch limit reached ({n}). Send {p}unwatch <n> to stop one first.",
            "关注数已达上限（{n} 个）。请先 {p}unwatch <编号> 取消部分关注。",
        ),
        // /watch 无参：agent 列表（同 /status）+ 选择提示 + 已关注段标题。
        "watch.pickHint" => pick(
            lang,
            "Send {p}watch <n> to follow one with a live status card.",
            "发送 {p}watch <编号> 即可用一张实时状态卡关注该 agent。",
        ),
        "watch.pickHintWorkingOnly" => pick(
            lang,
            "Send {p}watch <n> to follow a working agent with a live status card.",
            "发送 {p}watch <编号> 可关注一个工作中的 Agent，获取实时状态卡片。",
        ),
        "watch.listTitle" => pick(lang, "Watching:", "正在关注："),
        // /unwatch：确认与提示。
        "watch.unwatchDone" => pick(lang, "Stopped watching [{id}].", "已取消关注 [{id}]。"),
        "watch.unwatchAllDone" => pick(lang, "Stopped watching all ({n}).", "已取消全部关注（{n} 个）。"),
        "watch.unwatchNone" => pick(lang, "Not watching any agent.", "当前没有关注任何 agent。"),
        "watch.unwatchWhich" => pick(
            lang,
            "Watching more than one agent — send {p}unwatch <n> to pick one, or {p}unwatch all:",
            "正在关注多个 agent，请用 {p}unwatch <编号> 指定，或 {p}unwatch all 全部取消：",
        ),
        "watch.notWatching" => pick(
            lang,
            "Not watching agent [{id}]. Send {p}watch to list current watches.",
            "没有关注编号为 {id} 的 agent。发送 {p}watch 查看当前关注。",
        ),
        // 发卡失败（飞书配置/网络问题）。
        "watch.sendFailed" => pick(lang, "Failed to send the watch card: {e}", "发送关注卡片失败：{e}"),
        // 卡片：样式化头部（{id} 编号、{agent} 家族名、{project} 项目名）。
        "watch.cardHeader" => pick(lang, "Watching [{id}] {agent} — {project}", "实时关注 [{id}] {agent} — {project}"),
        // Cumulative active time across turns; true Idle intervals are excluded.
        "watch.statsActiveElapsed" => pick(lang, "worked {t} total", "累计工作 {t}"),
        // TODO 摘要（`/status` 尾行 / watch 卡折叠面板标题；agent 未用 todo 功能不显示）。
        // 「TODO」不翻译（用户定案）。
        "watch.todoSummary" => pick(
            lang,
            "📋 TODO {done}/{total} · now: {current}",
            "📋 TODO {done}/{total} · 当前：{current}",
        ),
        "watch.todoSummaryBare" => pick(lang, "📋 TODO {done}/{total}", "📋 TODO {done}/{total}"),
        // 足迹时间线「省略 N 步」标注（文字与展示的 ≤3 步之间还有更早调用时）。
        "watch.stepsOmitted" => pick(lang, "… {n} earlier steps omitted", "… 已省略 {n} 步"),
        // 卡片状态行（emoji 编码四态；waiting 覆盖 working）。
        "watch.stateWorking" => pick(lang, "🟢 Working", "🟢 工作中"),
        "watch.stateIdle" => pick(lang, "⚪ Idle", "⚪ 空闲"),
        "watch.stateWaiting" => pick(lang, "🙋 Waiting for your answer", "🙋 正在等待你的回答"),
        "watch.stateEnded" => pick(lang, "⏹ Ended", "⏹ 已结束"),
        // 卡片底部更新时刻（{time} 本地绝对时刻）。
        "watch.updatedAt" => pick(lang, "Updated {time}", "最后更新 {time}"),
        // 卡片按钮。
        "watch.btnUnwatch" => pick(lang, "Unwatch", "取消关注"),
        "watch.btnRefresh" => pick(lang, "Refresh", "立即刷新"),
        "watch.forkedFrom" => pick(lang, "forked from {id}", "从 {id} 分叉"),
        // 终态按钮（禁用）。
        "watch.btnEnded" => pick(lang, "Ended · auto-unwatched", "已结束 · 已自动取消关注"),
        "watch.btnCancelled" => pick(lang, "Unwatched", "已取消关注"),
        "watch.btnReplaced" => pick(lang, "Replaced by a newer card", "已由新卡片接替"),
        "watch.btnMoved" => pick(lang, "Moved to the latest card ⬇", "已移至最新卡片 ⬇"),
        "watch.btnIdle" => pick(lang, "Idle · auto-unwatched", "已空闲 · 已自动取消关注"),
        "watch.btnAutoStopped" => {
            pick(lang, "Auto-stopped (switched to {to})", "已切换到 {to} · 自动结束关注")
        }
        // 可重新关注（可点击，非 disabled）。
        "watch.btnRewatch" => {
            pick(lang, "Switched to {to} · Click to re-watch", "已切换到 {to} · 点击重新关注")
        }
        "watch.btnRewatchCancelled" => {
            pick(lang, "Unwatched · Click to re-watch", "已取消关注 · 点击重新关注")
        }
        // 已完成重新关注（disabled）。
        "watch.btnRewatched" => pick(lang, "⬇ Re-watched in new card", "⬇ 已在新卡片重新关注"),

        // —— 通用「单选卡」（spec docs/specs/im-select-card.md）——
        // 卡片标题（按命令种类）。
        "select.titleWatch" => pick(
            lang,
            "Pick an agent to watch (one tap starts):",
            "选择要实时关注的 Agent（点一下即开始）：",
        ),
        "select.titleStatus" => pick(lang, "Pick an agent to view:", "选择要查看的 Agent："),
        "select.titleUnwatch" => pick(lang, "Pick an agent to unwatch:", "选择要取消关注的 Agent："),
        // `/msg` 无编号选择卡标题（发送插话；仅列工作中·非 grok）。
        "select.titleMsg" => pick(
            lang,
            "Pick an agent to message:",
            "选择要发送消息的 Agent：",
        ),
        // 每行触发按钮文案（按动作种类）。
        "select.btnWatch" => pick(lang, "Watch", "关注"),
        "select.btnFork" => pick(lang, "Fork", "分叉"),
        "select.titleFork" => pick(lang, "Choose a session to fork", "选择要分叉的会话"),
        "select.btnStatus" => pick(lang, "View", "查看"),
        "select.btnUnwatch" => pick(lang, "Unwatch", "取消"),
        "select.btnMsg" => pick(lang, "Send", "发送"),
        "select.btnDiff" => pick(lang, "Diff", "差异"),
        "select.btnStage" => pick(lang, "Stage", "暂存"),
        "select.btnTranscript" => pick(lang, "Transcript", "会话"),
        "select.btnChoose" => pick(lang, "Choose", "选择"),
        "select.btnTodo" => pick(lang, "Todos", "待办"),
        "select.btnTodoRmEntry" => pick(lang, "Delete", "删除"),
        "select.titleDiff" => pick(
            lang,
            "Pick an agent for unstaged diff:",
            "选择要查看未暂存 diff 的 Agent：",
        ),
        "select.titleStage" => pick(
            lang,
            "Pick an agent to stage changes:",
            "选择要暂存改动的 Agent：",
        ),
        "select.titleTranscript" => pick(
            lang,
            "Pick an agent for full transcript:",
            "选择要导出完整会话的 Agent：",
        ),
        // —— /todo · /todo-rm（spec todo-whats-next D8）——
        "select.titleTodo" => pick(
            lang,
            "Pick a project to open its todos:",
            "选择要查看待办的项目：",
        ),
        "select.titleTodoRm" => pick(
            lang,
            "Pick a project to delete its todos:",
            "选择要删除待办的项目：",
        ),
        "select.titleTodoRmEntries" => pick(
            lang,
            "Todos of \"{project}\" (tap Delete to remove):",
            "「{project}」的待办（点删除即移除）：",
        ),
        "select.todoRmAllDoneCard" => pick(
            lang,
            "All todos in this project are gone.",
            "本项目待办已全部删除/清空。",
        ),
        "select.todoRmDeleted" => pick(lang, "Deleted: {text}", "已删除：{text}"),
        // —— /yolo（spec codex-permission-remember D53）——
        "select.titleYolo" => pick(
            lang,
            "Sessions with YOLO mode on (tap to end YOLO mode):",
            "开启 YOLO 模式的会话（点关闭即结束 YOLO）：",
        ),
        "select.btnYoloOff" => pick(lang, "Turn off", "关闭"),
        "select.yoloNone" => pick(
            lang,
            "No session has YOLO mode on.",
            "当前没有开启 YOLO 模式的会话。",
        ),
        "select.yoloOffDone" => pick(
            lang,
            "YOLO mode turned off: {name}",
            "已关闭 YOLO 模式：{name}",
        ),
        "select.yoloOffGone" => pick(
            lang,
            "YOLO mode was already off for this session.",
            "该会话的 YOLO 模式已不在开启状态。",
        ),
        "select.yoloOffHint" => pick(
            lang,
            "Send {p}yolo off [n] to turn it off anytime.",
            "发送 {p}yolo off [编号] 可随时关闭。",
        ),
        // `/todo-auto`（第 17 轮定案）。
        "select.btnTodoAutoEntry" => pick(lang, "Toggle", "切换"),
        "select.titleTodoAuto" => pick(
            lang,
            "Pick a project to toggle its auto-run todos:",
            "选择要切换待办自动执行的项目：",
        ),
        "select.titleTodoAutoEntries" => pick(
            lang,
            "Todos of \"{project}\" (tap Toggle to switch auto-run; ⚡ = auto):",
            "「{project}」的待办（点切换开/关自动执行；⚡＝已自动）：",
        ),
        "select.todoAutoEmptyCard" => pick(
            lang,
            "No pending todos in this project.",
            "本项目暂无待办。",
        ),
        // 管理卡 / 直达追加的文本回执。
        "todoIm.added" => pick(
            lang,
            "Added to \"{project}\" todos ({n} pending).",
            "已加入「{project}」待办（现有 {n} 条）。",
        ),
        "todoIm.listTitle" => pick(lang, "Todos of \"{project}\"", "「{project}」的待办"),
        "todoIm.empty" => pick(lang, "(no pending todos)", "（暂无待办）"),
        "todoIm.addHint" => pick(
            lang,
            "Add: send {p}todo <text> and choose this project; delete: {p}todo-rm.",
            "新增：发送 {p}todo <内容> 后选择本项目；删除：发送 {p}todo-rm。",
        ),
        // 卡片自带输入框（飞书/钉钉）时正文只提示删除入口。
        "todoIm.rmHint" => pick(
            lang,
            "Delete: send {p}todo-rm and choose this project.",
            "删除：发送 {p}todo-rm 后选择本项目。",
        ),
        "todoIm.cardInputPlaceholder" => pick(
            lang,
            "Type a new todo and submit to add",
            "输入新待办，提交即新增",
        ),
        "todoIm.cardAddButton" => pick(lang, "Add todo", "新增待办"),
        "todoIm.autoCardInputPlaceholder" => pick(
            lang,
            "Type a new auto-run todo",
            "输入新的自动待办",
        ),
        "todoIm.autoCardAddButton" => pick(
            lang,
            "Add auto-run todo",
            "新增自动待办",
        ),
        "todoIm.autoAddTitle" => pick(
            lang,
            "Add auto-run todo to \"{project}\"",
            "为「{project}」新增自动待办",
        ),
        "todoIm.autoAddCardHint" => pick(
            lang,
            "New entries are marked auto-run ⚡ and will be dispatched directly by whats-next.",
            "新增条目会标记为自动执行 ⚡，whats-next 时将直接派发。",
        ),
        "todoIm.autoAddHint" => pick(
            lang,
            "Add an auto-run todo: send {p}todo-auto <text> and choose this project.",
            "新增自动待办：发送 {p}todo-auto <内容> 后选择本项目。",
        ),
        "todoIm.noAgents" => pick(
            lang,
            "No live agent sessions; cannot locate a project. Todos can still be managed via the popup, tray window, or `AskHuman todo`.",
            "当前没有存活的 Agent 会话，无法定位项目。仍可在弹窗、托盘待办窗口或 `AskHuman todo` 命令中管理待办。",
        ),
        "todoIm.noProjects" => pick(
            lang,
            "No projects are available from live Agents, recent workspaces, or saved todos.",
            "没有可选项目（活跃 Agent、最近工作目录和已有待办中均未找到）。",
        ),
        "todoIm.projectPickerFailed" => pick(
            lang,
            "Failed to send the project picker. Please try again.",
            "项目选择卡发送失败，请重试。",
        ),
        "todoIm.projectTodoBadge" => pick(lang, "· {n} todo(s)", "· {n} 条待办"),
        "todoIm.projectPicked" => pick(
            lang,
            "Selected project: {project}",
            "已选择项目：{project}",
        ),
        "todoIm.addFailed" => pick(lang, "Failed to add the todo.", "新增待办失败。"),
        "todoIm.notFound" => pick(
            lang,
            "No agent with number {n}. Send {p}status to list.",
            "没有编号为 {n} 的 agent。发送 {p}status 查看列表。",
        ),
        "todoIm.noProject" => pick(
            lang,
            "That agent has no working directory; cannot locate its project.",
            "该 Agent 没有工作目录，无法定位项目。",
        ),
        "todoIm.rmEmpty" => pick(
            lang,
            "No pending todos in \"{project}\".",
            "「{project}」当前没有待办。",
        ),
        "todoIm.usage" => pick(
            lang,
            "Usage: {p}todo [text] — choose a project to open its todo card, or add the supplied text after choosing.",
            "用法：{p}todo [内容]——选择项目打开待办卡；带内容时选择后直接新增。",
        ),
        "todoIm.addedAuto" => pick(
            lang,
            "Added to \"{project}\" todos as auto-run ⚡ ({n} pending).",
            "已加入「{project}」待办并设为自动执行 ⚡（现有 {n} 条）。",
        ),
        "todoIm.usageAuto" => pick(
            lang,
            "Usage: {p}todo-auto [text] — choose a project to toggle auto-run, or add the supplied text as an auto-run todo after choosing.",
            "用法：{p}todo-auto [内容]——选择项目切换自动执行；带内容时选择后新增自动待办。",
        ),
        "select.diffDoneCard" => pick(lang, "Diff sent for [{id}]", "已发送 [{id}] 的 diff"),
        "select.stageOpenedCard" => pick(
            lang,
            "Confirm card opened for [{id}]",
            "已为 [{id}] 打开暂存确认",
        ),
        "select.transcriptDoneCard" => pick(
            lang,
            "Transcript sent for [{id}]",
            "已发送 [{id}] 的会话记录",
        ),
        // 选项徽标：已在本渠道关注中（`/watch` 卡；点它＝换新卡）。前后空格由渲染器按需拼接。
        "select.watchingBadge" => pick(lang, "· watching", "· 关注中"),
        // —— Confirm 卡（/stage）——
        "confirm.stageTitle" => pick(lang, "Stage changes · {project}", "暂存改动 · {project}"),
        "confirm.stageIntro" => pick(
            lang,
            "About to `git add -A` **{n}** path(s):",
            "即将对 **{n}** 个路径执行 `git add -A`：",
        ),
        "confirm.stageMore" => pick(lang, "…and {n} more", "…另有 {n} 个"),
        "confirm.btnConfirm" => pick(lang, "Confirm stage", "确认暂存"),
        "confirm.btnCancel" => pick(lang, "Cancel", "取消"),
        "confirm.stageDone" => pick(lang, "Staged {n} path(s).", "已暂存 {n} 个路径。"),
        "confirm.stageCancelled" => pick(lang, "Staging cancelled.", "已取消暂存。"),
        "confirm.stageChanged" => pick(
            lang,
            "Working tree changed since the confirm card — run /stage again.",
            "工作区已变化，请重新发送 /stage。",
        ),
        "confirm.stageFailed" => pick(lang, "git add failed: {err}", "git add 失败：{err}"),
        // —— /diff · /stage · /transcript 文本 ——
        "export.notFound" => pick(
            lang,
            "No agent with number {n}. Send {p}status to list.",
            "没有编号为 {n} 的 agent。发送 {p}status 查看列表。",
        ),
        "export.noCwd" => pick(lang, "This agent has no working directory.", "该 agent 没有工作目录。"),
        "export.notGit" => pick(
            lang,
            "Not a git repository (no .git above {path}).",
            "不是 git 仓库（{path} 之上没有 .git）。",
        ),
        "export.noUnstaged" => pick(lang, "No unstaged changes.", "没有未暂存的改动。"),
        "export.diffSummary" => pick(
            lang,
            "[{n}] {kind} · {project} · unstaged diff · {files} file(s)",
            "[{n}] {kind} · {project} · 未暂存 diff · {files} 个文件",
        ),
        "export.transcriptSummary" => pick(
            lang,
            "[{n}] {kind} · {project} · transcript",
            "[{n}] {kind} · {project} · 会话记录",
        ),
        "export.noTranscript" => pick(
            lang,
            "Could not find transcript for this agent session.",
            "找不到该 agent 会话的 transcript 文件。",
        ),
        "export.sendFailed" => pick(lang, "Failed to send file: {err}", "发送文件失败：{err}"),
        // 选项超上限截断说明（{n} = 实际展示数）。
        "select.truncated" => pick(lang, "(showing first {n})", "（仅列前 {n} 个）"),
        // graceful 关停时活动卡片的定格文案（第 15 轮定案：daemon 台账不持久化，
        // 退出前主动去掉按钮并留提示，避免重启后旧卡静默无响应）。
        "select.expiredCard" => pick(
            lang,
            "This card has expired (service restarted). Send the command again.",
            "卡片已失效（服务已重启），请重新发送命令。",
        ),
        // `/unwatch` 单选卡取到 0 个后定格文案。
        "select.unwatchAllDoneCard" => pick(lang, "All unwatched.", "已全部取消关注。"),
        // 钉钉 `/watch` 单选卡点选后定格文案（{id} = 所选 agent 展示编号）：钉钉不能就地变身，
        // 单选卡定格为「已选择 [n]」、另发一张新的实时 watch 卡。
        "select.pickedCard" => pick(lang, "Selected [{id}]", "已选择 [{id}]"),
        // `/msg` 选择卡点「发送」后定格（{id} = 展示编号；{note} = 送达/排队回执）。
        "select.msgSentCard" => pick(lang, "Sent to [{id}] · {note}", "已发送给 [{id}] · {note}"),
        // 点「发送」瞬间目标已不在工作中（状态漂移）→ 定格提示、不发送。
        "select.msgTargetGone" => pick(
            lang,
            "That agent is no longer working; not sent.",
            "该 Agent 已不在工作中，未发送。",
        ),
        // `/msg` 无编号但当前没有可发送对象（无工作中·非 grok）。
        "select.msgNoWorking" => pick(
            lang,
            "No working agents to message right now.",
            "当前没有工作中的 Agent，无法发送。",
        ),
        // 显式 `/msg <编号>` 发送但目标不在工作中（仅工作中可发）。
        "select.msgNoWorkingTarget" => pick(
            lang,
            "That agent is idle; you can only message a working agent.",
            "该 Agent 当前空闲，只能给工作中的 Agent 发送。",
        ),

        // —— Slack 渠道：发给用户的文案 + 本地诊断 ——
        // 静态终态卡片状态行（无 emoji 前缀，与飞书/钉钉一致）。
        "channel.slSubmitted" => pick(lang, "Submitted", "已提交"),
        "channel.slAnsweredVia" => pick(lang, "Answered via {source}", "已在{source}回答"),
        "channel.slCancelled" => pick(lang, "Cancelled", "已取消"),
        "channel.slCancelledBy" => pick(lang, "Cancelled by {source}", "已被{source}取消"),
        "channel.slTitleFallback" => pick(lang, "Question", "提问"),
        // 卡片表单标签 / 占位 / 按钮。
        "channel.slOptionsLabel" => pick(lang, "Options", "选项"),
        "channel.slInputLabel" => pick(lang, "Note", "补充说明"),
        "channel.slInputPlaceholder" => pick(lang, "Add a note (optional)", "补充说明（可选）"),
        "channel.slSubmitButton" => pick(lang, "Submit", "提交"),
        "channel.slConfigInvalidSkip" => pick(
            lang,
            "invalid Slack config, skipping this channel: {e}",
            "Slack 配置无效，已跳过该 Channel: {e}",
        ),
        "channel.slMessageSendFailed" => pick(lang, "failed to send Slack Message: {e}", "Slack Message 发送失败: {e}"),
        "channel.slFileSendFailedLog" => pick(lang, "failed to send Slack file: {path}: {e}", "Slack 文件发送失败: {path}: {e}"),
        "channel.slQuestionSendFailed" => pick(lang, "failed to send Slack question: {e}", "Slack 提问发送失败: {e}"),
        "channel.slCardDeliverFailed" => pick(lang, "failed to deliver Slack card, falling back to text: {e}", "Slack 互动卡片投放失败，回退纯文本: {e}"),
        "channel.slFileDownloadFailed" => pick(lang, "failed to download Slack file: {e}", "Slack 文件下载失败: {e}"),

        // —— 设置页「弹出测试窗口」示例内容 ——
        "test.message" => pick(
            lang,
            "This is a test popup for previewing the appear animation and appearance.",
            "这是一个测试弹窗，用于预览弹出动画与外观。",
        ),
        "test.questionAppearance" => pick(lang, "How does the popup look?", "弹窗效果看起来如何？"),
        "test.optionGood" => pick(lang, "Looks good", "很好"),
        "test.optionAdjust" => pick(lang, "Needs tweaks", "再调整"),
        "test.questionAnimation" => pick(lang, "Does the appear animation feel smooth?", "弹出动画是否流畅？"),
        "test.optionSmooth" => pick(lang, "Smooth", "流畅"),
        "test.optionLaggy" => pick(lang, "A little laggy", "有些卡顿"),
        "test.questionSuggestions" => pick(lang, "Any other suggestions?", "还有其他建议吗？"),

        // —— IPC 命令直接返回的 GUI 文案（commands.rs）——
        "cmd.invalidAttachmentIndex" => pick(lang, "Invalid attachment index", "无效的附件索引"),
        "cmd.readFileFailed" => pick(lang, "Failed to read file: {e}", "读取文件失败: {e}"),
        "cmd.fileIconUnsupported" => pick(
            lang,
            "Getting a file icon is not supported on this platform",
            "当前平台不支持获取文件图标",
        ),
        "cmd.openFailed" => pick(lang, "Failed to open: {e}", "打开失败: {e}"),
        "cmd.locateExeFailed" => pick(lang, "Failed to locate the program path: {e}", "无法定位程序路径: {e}"),
        "cmd.testPopupFailed" => pick(lang, "Failed to launch the test popup: {e}", "启动测试弹窗失败: {e}"),
        "cmd.hookInstalled" => pick(lang, "Cursor Hook installed", "已安装 Cursor Hook"),
        "cmd.hookUpdated" => pick(lang, "Hook updated to the latest version", "已更新到最新版本"),
        "cmd.hookRemoved" => pick(lang, "Cursor Hook removed", "已移除 Cursor Hook"),
        "cmd.ruleInstalled" => pick(lang, "Rule written", "已写入规则文件"),
        "cmd.ruleUpdated" => pick(lang, "Prompt updated to the latest version", "已更新到最新提示词"),
        "cmd.ruleRemoved" => pick(lang, "Rule removed", "已从规则文件移除"),
        "cmd.skillInstalled" => pick(lang, "Skill written", "已写入 skill 文件"),
        "cmd.skillUpdated" => pick(lang, "Skill updated to the latest version", "已更新 skill 到最新"),
        "cmd.skillRemoved" => pick(lang, "Skill removed", "已移除 skill 文件"),
        "cmd.unknownAgent" => pick(lang, "Unknown agent", "未知的 Agent"),
        "cmd.unknownMode" => pick(lang, "Unknown mode", "未知的模式"),
        "cmd.unknownArtifact" => pick(lang, "Unknown artifact", "未知的产物"),
        "cmd.lifecycleInstalled" => pick(lang, "Lifecycle tracking enabled", "已开启生命周期追踪"),
        "cmd.lifecycleRemoved" => pick(lang, "Lifecycle tracking disabled", "已关闭生命周期追踪"),
        "cmd.mcpConfigInstalled" => pick(lang, "MCP config written", "已写入 MCP 配置"),
        "cmd.mcpConfigUpdated" => pick(lang, "MCP config updated to the latest version", "已更新 MCP 配置到最新"),
        "cmd.mcpConfigRemoved" => pick(lang, "MCP config removed", "已移除 MCP 配置"),

        // —— Telegram 测试连接（commands.telegram_test / test_connection）——
        "cmd.tgTestRemote" => pick(
            lang,
            "🤖 AskHuman test message\n\nThis is a test message — your Telegram Bot is configured correctly!",
            "🤖 AskHuman 测试消息\n\n这是一条测试消息，表示 Telegram Bot 配置成功！",
        ),
        "cmd.tgTestSent" => pick(
            lang,
            "Test message sent! Your Telegram Bot is configured correctly.",
            "测试消息发送成功！Telegram Bot 配置正确。",
        ),

        // —— 钉钉测试连接 / 自动识别（commands.dingtalk_test / dingtalk_detect_*）——
        "cmd.fillUserId" => pick(
            lang,
            "Please fill in UserId first (use “Auto-detect” to get it)",
            "请先填写 UserId（可点击「自动识别」获取）",
        ),
        "cmd.ddTestRemote" => pick(
            lang,
            "✅ AskHuman DingTalk connection test succeeded",
            "✅ AskHuman 钉钉连接测试成功",
        ),
        "cmd.ddTestSent" => pick(
            lang,
            "A test message was sent to your direct chat — please check DingTalk",
            "已向你的单聊发送一条测试消息，请在钉钉查收",
        ),
        "cmd.fillClientIdSecret" => pick(
            lang,
            "Please fill in ClientId and ClientSecret first",
            "请先填写 ClientId 和 ClientSecret",
        ),
        "cmd.detectCodeInvalid" => pick(lang, "Invalid detect code, please retry", "识别码无效，请重试"),
        "cmd.detectTimeout" => pick(
            lang,
            "Timed out after 120s without a matching detect code, please retry",
            "等待超时（120 秒）未收到匹配的识别码，请重试",
        ),
        "cmd.streamDisconnected" => pick(lang, "Stream disconnected, please retry", "Stream 连接断开，请重试"),
        "cmd.detectCancelled" => pick(lang, "Auto-detect cancelled", "已取消自动识别"),

        // —— 飞书测试连接 / open_id 自动识别（commands.feishu_test / feishu_detect_*）——
        "cmd.fillOpenId" => pick(
            lang,
            "Please fill in Open ID first (use “Auto-detect” to get it)",
            "请先填写 Open ID（可点击「自动识别」获取）",
        ),
        "cmd.fsTestRemote" => pick(
            lang,
            "✅ AskHuman Feishu connection test succeeded",
            "✅ AskHuman 飞书连接测试成功",
        ),
        "cmd.fsTestSent" => pick(
            lang,
            "A test message was sent to your direct chat — please check Feishu",
            "已向你的单聊发送一条测试消息，请在飞书查收",
        ),
        "cmd.fillAppIdSecret" => pick(
            lang,
            "Please fill in AppId and AppSecret first",
            "请先填写 AppId 和 AppSecret",
        ),

        // —— Slack 测试连接 / userId 自动识别（commands.slack_test / slack_detect_*）——
        "cmd.slTestRemote" => pick(
            lang,
            "✅ AskHuman Slack connection test succeeded",
            "✅ AskHuman Slack 连接测试成功",
        ),
        "cmd.slTestSent" => pick(
            lang,
            "A test message was sent to your direct message — please check Slack",
            "已向你的单聊发送一条测试消息，请在 Slack 查收",
        ),
        "cmd.fillSlackTokens" => pick(
            lang,
            "Please fill in Bot Token and App Token first",
            "请先填写 Bot Token 和 App Token",
        ),

        // —— 菜单栏 / 托盘（GUI 宿主，spec D7）——
        // 状态区（只读）。
        "tray.running" => pick(lang, "AskHuman — running", "AskHuman — 运行中"),
        "tray.stopped" => pick(lang, "AskHuman — not running", "AskHuman — 未运行"),
        "tray.version" => pick(lang, "Version {v}", "版本 {v}"),
        "tray.uptime" => pick(lang, "Up {d}", "已运行 {d}"),
        "tray.draining" => pick(lang, "Finishing in-flight requests…", "正在完成在途请求…"),
        "tray.pendingQuestions" => pick(lang, "{n} pending question(s)", "{n} 个待答"),
        "tray.pendingUntitled" => pick(lang, "(no preview)", "（无预览）"),
        "tray.imConnections" => pick(lang, "Channels: {list}", "渠道：{list}"),
        // 渠道故障警示行（R7）：可点击，打开设置渠道 tab 查看错误详情。
        "tray.channelIssue" => pick(lang, "⚠ {ch} issue ({t})", "⚠ {ch}异常（{t}）"),
        "tray.integrationUpdateOne" => pick(
            lang,
            "💡 {agent} integration needs an update",
            "💡 {agent} 集成需更新",
        ),
        "tray.integrationUpdateMany" => pick(
            lang,
            "💡 {count} Agent integrations need updates",
            "💡 {count} 个 Agent 集成需更新",
        ),
        "tray.integrationUpdatesTooltip" => pick(
            lang,
            "Agent integrations need updates",
            "Agent 集成需更新",
        ),
        "tray.justNow" => pick(lang, "just now", "刚刚"),
        "tray.minutesAgo" => pick(lang, "{n} min ago", "{n} 分钟前"),
        "tray.hoursAgo" => pick(lang, "{n} h ago", "{n} 小时前"),
        "tray.updateAvailable" => pick(lang, "● Update available ({v})", "● 有可用更新（{v}）"),
        "tray.updateTargetFallback" => pick(lang, "New version", "新版本"),
        "tray.updatePendingWaiting" => pick(
            lang,
            "↑ {v} installed — waiting for {n} in-flight request(s) before restarting daemon (won't interrupt them)",
            "↑ {v} 已安装 — 等待 {n} 个在途请求完成后重启 daemon（不会中断作答）",
        ),
        "tray.updatePendingRestarting" => pick(
            lang,
            "↑ {v} installed — restarting daemon…",
            "↑ {v} 已安装 — daemon 正在重启…",
        ),
        "tray.updatePendingSwitching" => pick(
            lang,
            "↑ {v} installed — waiting for daemon to restart…",
            "↑ {v} 已安装 — 正在等待 daemon 重启…",
        ),
        "tray.updatePendingNextStart" => pick(
            lang,
            "↑ {v} installed — updated daemon starts on next use",
            "↑ {v} 已安装 — 下次使用时启动新版 daemon",
        ),
        // 操作区。
        "tray.openSettings" => pick(lang, "Settings", "设置"),
        "tray.openHistory" => pick(lang, "History", "历史记录"),
        "tray.openTodos" => pick(lang, "Todos", "待办"),
        "tray.newTask" => pick(lang, "New Agent Task", "新建 Agent 任务"),
        "tray.openAgents" => pick(lang, "Open Agent Status Window", "打开 Agent 状态窗口"),
        // Agent 子菜单（spec agent-interject D7）：标签即忙闲概览。
        "tray.agentOverview" => pick(
            lang,
            "{w} working · {i} idle",
            "工作中 {w} · 空闲 {i}",
        ),
        "tray.agentOpenConsole" => pick(lang, "View in Console", "在控制台查看"),
        "tray.agentFork" => pick(lang, "Fork session", "Fork 会话"),
        "tray.agentSendMessage" => pick(lang, "Send Message…", "发送消息…"),
        "tray.agentSendMessagePending" => pick(
            lang,
            "Send Message… (queued)",
            "发送消息…（有待送达）",
        ),
        "tray.agentFocusTerminal" => pick(lang, "Focus Terminal", "聚焦终端"),
        // Agent 子菜单「添加待办」：打开待办窗口并预选该 agent 的项目、聚焦新增输入框。
        "tray.agentAddTodo" => pick(lang, "Add Todo…", "添加待办…"),
        "tray.agentAskNow" => pick(lang, "Ask Me Now", "要求提问"),
        "tray.checkUpdate" => pick(lang, "Check for Updates", "检查更新"),
        "tray.checkingUpdate" => pick(lang, "Checking for updates…", "正在检查更新…"),
        "tray.updateCurrent" => pick(lang, "AskHuman is up to date", "AskHuman 已是最新版"),
        "tray.updateFound" => pick(lang, "Update found: v{v}", "已检查到新版本 v{v}"),
        "tray.checkUpdateFailed" => pick(
            lang,
            "⚠ Update check failed: {e}",
            "⚠ 检查更新失败：{e}",
        ),
        "tray.applyUpdate" => pick(
            lang,
            "Update to v{v} (applies after answering)",
            "更新到 v{v}（答完后生效）",
        ),
        "tray.prepareManualUpdate" => pick(
            lang,
            "Prepare manual update to v{v}…",
            "准备手动更新至 v{v}…",
        ),
        "tray.applyingUpdate" => pick(lang, "Updating AskHuman…", "正在更新 AskHuman…"),
        "tray.applyUpdateFailed" => pick(
            lang,
            "⚠ Update failed: {e}",
            "⚠ 更新失败：{e}",
        ),
        // 盘上二进制已换新但窗口开着（自动换新被挡）时的宿主重启项（B2）。
        "tray.restartHost" => pick(
            lang,
            "Restart Menu Bar App to Finish Update",
            "重启菜单栏应用以完成更新",
        ),
        "tray.restartHostFailed" => pick(
            lang,
            "⚠ Menu Bar App restart failed: {e}",
            "⚠ 菜单栏应用重启失败：{e}",
        ),
        "tray.startDaemon" => pick(lang, "Start Daemon", "启动 daemon"),
        "tray.restartDaemon" => pick(lang, "Restart Daemon", "重启 daemon"),
        "tray.stopDaemon" => pick(lang, "Stop Daemon", "停止 daemon"),
        // 有「工作中」agent 时隐藏「停止」项（停了也会被 hook/下次 ask 立即拉起），代以本行灰色说明。
        "tray.stopDaemonBlocked" => pick(
            lang,
            "An agent is working — Daemon stays active",
            "有 Agent 工作中，Daemon 将保持激活",
        ),
        "tray.quit" => pick(lang, "Quit Menu Bar Icon", "退出菜单栏图标"),
        // tooltip 概要。
        "tray.tooltipRunning" => pick(lang, "AskHuman — running", "AskHuman — 运行中"),
        "tray.tooltipStopped" => pick(lang, "AskHuman — not running", "AskHuman — 未运行"),

        // —— 错误类型校验文案（Telegram/钉钉/飞书 Error::localized）——
        "err.tgEmptyToken" => pick(lang, "Bot Token must not be empty", "Bot Token 不能为空"),
        "err.tgEmptyChatId" => pick(lang, "Chat ID must not be empty", "Chat ID 不能为空"),
        "err.tgInvalidChatId" => pick(
            lang,
            "Invalid Chat ID format; enter a valid numeric ID",
            "Chat ID 格式无效，请输入有效的数字 ID",
        ),
        "err.ddEmptyConfig" => pick(lang, "{field} must not be empty", "{field} 不能为空"),
        "err.fsEmptyConfig" => pick(lang, "{field} must not be empty", "{field} 不能为空"),
        "err.slEmptyConfig" => pick(lang, "{field} must not be empty", "{field} 不能为空"),

        _ => key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_overrides_system() {
        assert_eq!(Lang::resolve("en"), Lang::En);
        assert_eq!(Lang::resolve("zh"), Lang::Zh);
    }

    #[test]
    fn auto_or_unknown_follows_system() {
        // 仅验证不 panic 且落在二者之一。
        let a = Lang::resolve("auto");
        let b = Lang::resolve("nonsense");
        assert!(matches!(a, Lang::En | Lang::Zh));
        assert!(matches!(b, Lang::En | Lang::Zh));
    }
}
