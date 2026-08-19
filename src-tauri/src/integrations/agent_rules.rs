//! Agent 全局提示词（Rules）安装/卸载/更新/状态。
//!
//! Agent families share one prompt template. Rendering may add a target-specific scope rule before
//! writing the owned `begin/end` block; content outside the block is always preserved. Targets:
//! - Cursor：`~/.cursor/rules/askhuman.mdc`（`alwaysApply` frontmatter + 托管区块）。
//! - Claude Code：`~/.claude/CLAUDE.md` 内的托管区块。
//! - Codex：`~/.codex/AGENTS.md` 内的托管区块。
//! - Pi：`~/.pi/agent/AGENTS.md` 内的托管区块。
//!
//! 「更新」用于内置提示词随版本变化后，把已安装的旧正文覆盖为最新（仅替换区块内部）。
//! Cursor 卸载时若区块外只剩 frontmatter / 空白则删除整个文件，否则保留用户内容。
//! 旧版 Cursor 独占文件（含 `MANAGED_FILE_MARK` 头标记、无区块）仍识别为已安装，并在
//! 安装 / 更新时整体迁移为新的区块格式。区块增删为纯函数，便于单测（幂等、保留他人内容）。

use crate::paths;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// 共享文件托管区块起始标记。
pub const BLOCK_BEGIN: &str = "<!-- AskHuman:begin DO NOT EDIT (managed by AskHuman) -->";
/// 共享文件托管区块结束标记。
pub const BLOCK_END: &str = "<!-- AskHuman:end -->";
/// 旧版 Cursor 独占文件头标记（仅用于识别 / 迁移历史安装，新格式不再写入）。
pub const MANAGED_FILE_MARK: &str =
    "<!-- AskHuman:managed-file DO NOT EDIT (managed by AskHuman) -->";

/// Cursor 规则文件的 frontmatter（令规则始终生效）。
pub const CURSOR_FRONTMATTER: &str = "---\nalwaysApply: true\n---\n";

/// 规则正文变体：对应 CLI 模式（`cli_reference`）或 MCP 模式（`mcp_reference`）的提示词。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Variant {
    Cli,
    Mcp,
}

impl Variant {
    /// Render the latest built-in body for this variant and agent target.
    pub fn body(self, agent: AgentTarget) -> String {
        match self {
            Variant::Cli => crate::prompts::cli_reference_for(agent.kind()),
            Variant::Mcp => crate::prompts::mcp_reference_for(agent.kind()),
        }
    }
}

/// 目标 Agent。
///
/// 注意 `Grok` 的「指令载体」不是 rules 文件，而是 `~/.grok/skills/interaction-protocol/SKILL.md`
/// （见 [`crate::integrations::grok_skill`]）：Grok 默认模型 Composer 不读全局 `~/.grok/AGENTS.md`，
/// 故本模块对 `Grok` 的所有指令查询 / 安装 / 卸载 / 打开一律**委托** `grok_skill`，让 `agent_mode`、
/// 命令层、CLI 可用同一套 `AgentTarget` 统一处理五家（Grok 的 `Variant` 无意义，恒按 MCP 语义）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AgentTarget {
    Cursor,
    ClaudeCode,
    Codex,
    Grok,
    Pi,
}

impl AgentTarget {
    /// 由前端传入的字符串解析（未知值返回 None）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "cursor" => Some(AgentTarget::Cursor),
            "claude" => Some(AgentTarget::ClaudeCode),
            "codex" => Some(AgentTarget::Codex),
            "grok" => Some(AgentTarget::Grok),
            "pi" => Some(AgentTarget::Pi),
            _ => None,
        }
    }

    /// 该 target 的指令载体是否为 Grok skill（委托 `grok_skill` 而非本模块的 rules 机制）。
    fn is_grok_skill(self) -> bool {
        matches!(self, AgentTarget::Grok)
    }

    /// 目标规则文件路径（Grok 返回其 skill 文件路径，仅用于展示；实际读写走 `grok_skill`）。
    fn file(self) -> PathBuf {
        match self {
            AgentTarget::Cursor => paths::cursor_rule_file(),
            AgentTarget::ClaudeCode => paths::claude_md(),
            AgentTarget::Codex => paths::codex_agents_md(),
            AgentTarget::Grok => paths::grok_skill_md(),
            AgentTarget::Pi => paths::pi_agents_md(),
        }
    }

    /// 是否为「独占文件」模式（Cursor 为整文件拥有；其余为共享文件托管区块）。
    fn is_owned_file(self) -> bool {
        matches!(self, AgentTarget::Cursor)
    }

    fn kind(self) -> crate::agents::AgentKind {
        match self {
            AgentTarget::Cursor => crate::agents::AgentKind::Cursor,
            AgentTarget::ClaudeCode => crate::agents::AgentKind::Claude,
            AgentTarget::Codex => crate::agents::AgentKind::Codex,
            AgentTarget::Grok => crate::agents::AgentKind::Grok,
            AgentTarget::Pi => crate::agents::AgentKind::Pi,
        }
    }
}

// MARK: - 纯函数（可测试）

/// 共享文件是否已含本应用托管区块。
pub fn has_block(text: &str) -> bool {
    text.contains(BLOCK_BEGIN)
}

/// 在共享文件文本中插入/更新托管区块：已存在→替换其内部；不存在→追加到末尾（前置空行）。
/// 绝不改动区块以外的内容。
pub fn upsert_block(text: &str, body: &str) -> String {
    let block = format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}");
    if let Some((start, end)) = block_span(text) {
        let mut out = String::with_capacity(text.len() + block.len());
        out.push_str(&text[..start]);
        out.push_str(&block);
        out.push_str(&text[end..]);
        return out;
    }
    let base = text.trim_end();
    if base.is_empty() {
        format!("{block}\n")
    } else {
        format!("{base}\n\n{block}\n")
    }
}

/// 从共享文件文本中删除托管区块（含两行标记），并清理多余空行。不存在则原样返回。
pub fn remove_block(text: &str) -> String {
    if let Some((start, end)) = block_span(text) {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..start]);
        out.push_str(&text[end..]);
        return tidy(&out);
    }
    text.to_string()
}

/// 定位托管区块的字节区间 `[start, end)`（含 begin/end 两行标记本身）。
fn block_span(text: &str) -> Option<(usize, usize)> {
    let start = text.find(BLOCK_BEGIN)?;
    let end_marker = text[start..].find(BLOCK_END)? + start;
    Some((start, end_marker + BLOCK_END.len()))
}

/// 提取托管区块内的正文（begin/end 之间，去掉首尾换行）。不存在则 None。
pub fn block_body(text: &str) -> Option<String> {
    let start = text.find(BLOCK_BEGIN)? + BLOCK_BEGIN.len();
    let end = text[start..].find(BLOCK_END)? + start;
    Some(text[start..end].trim_matches('\n').to_string())
}

/// 折叠连续空行（最多保留一行空行）、去除尾部空白，非空时保留单个结尾换行。
fn tidy(s: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut prev_empty = false;
    for line in s.split('\n') {
        let is_empty = line.trim().is_empty();
        if is_empty && prev_empty {
            continue;
        }
        out.push(line);
        prev_empty = is_empty;
    }
    let trimmed = out.join("\n").trim_end().to_string();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// 组装 Cursor 规则文件内容：在旧文件基础上写入 frontmatter + 托管区块，保留区块外用户内容。
/// 旧版独占整文件（含头标记、无区块）整体迁移为新格式。
pub fn build_cursor_rule(old: &str, body: &str) -> String {
    if is_managed_cursor_file(old) && !has_block(old) {
        let block = format!("{BLOCK_BEGIN}\n{body}\n{BLOCK_END}");
        return format!("{CURSOR_FRONTMATTER}{block}\n");
    }
    upsert_block(&ensure_cursor_frontmatter(old), body)
}

/// 确保文本开头存在 frontmatter；缺失时前置 `alwaysApply` frontmatter。
fn ensure_cursor_frontmatter(text: &str) -> String {
    if text.trim().is_empty() {
        return CURSOR_FRONTMATTER.to_string();
    }
    if text.trim_start().starts_with("---") {
        return text.to_string();
    }
    format!("{CURSOR_FRONTMATTER}{text}")
}

/// 去掉开头的 YAML frontmatter（`---` 到下一个 `---`），返回其后的内容。无 frontmatter 原样返回。
fn strip_frontmatter(text: &str) -> &str {
    let trimmed = text.trim_start_matches('\n');
    if let Some(rest) = trimmed.strip_prefix("---\n") {
        if let Some(idx) = rest.find("\n---") {
            return &rest[idx + "\n---".len()..];
        }
    }
    text
}

/// 移除托管区块后，Cursor 文件的区块外是否只剩 frontmatter / 空白（可整文件删除）。
fn cursor_residual_is_empty(text: &str) -> bool {
    strip_frontmatter(text).trim().is_empty()
}

/// 旧版 Cursor 独占文件是否由本应用拥有（含头标记）。
pub fn is_managed_cursor_file(text: &str) -> bool {
    text.contains(MANAGED_FILE_MARK)
}

// MARK: - 状态 / 路径

/// 该 Agent 的规则是否已安装（新格式托管区块，或 Cursor 旧格式头标记）。
pub fn is_installed(agent: AgentTarget) -> bool {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::is_installed();
    }
    let path = agent.file();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    has_block(&text) || (agent.is_owned_file() && is_managed_cursor_file(&text))
}

/// 已安装但磁盘上的提示词正文与最新内置版本不一致 → 需更新（默认按 CLI 变体，保持现状语义）。
/// Cursor 旧格式（头标记、无区块）一律视为需更新（点更新即迁移为新格式）。
pub fn needs_update(agent: AgentTarget) -> bool {
    // Compare against the variant matching the current integration mode; a fixed CLI
    // comparison would permanently flag MCP-mode installs as outdated.
    let variant = match crate::integrations::agent_mode::current(agent) {
        crate::integrations::agent_mode::Mode::Mcp => Variant::Mcp,
        _ => Variant::Cli,
    };
    needs_update_variant(agent, variant)
}

/// 与指定变体的最新内置正文比对，判断是否需更新。
/// Cursor 旧格式（头标记、无区块）一律视为需更新。Grok 委托 skill 比对（忽略 variant）。
pub fn needs_update_variant(agent: AgentTarget, variant: Variant) -> bool {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::needs_update();
    }
    let Ok(text) = std::fs::read_to_string(agent.file()) else {
        return false;
    };
    if has_block(&text) {
        return block_body(&text)
            .map(|b| b != variant.body(agent))
            .unwrap_or(true);
    }
    agent.is_owned_file() && is_managed_cursor_file(&text)
}

/// 已安装规则的变体：区块正文精确匹配目标 Agent 的 MCP / CLI reference 即判定对应变体；
/// 漂移（旧版本提示词）时用结构性信号兜底（见 [`classify_body`]）。未安装返回 None。
pub fn installed_variant(agent: AgentTarget) -> Option<Variant> {
    if agent.is_grok_skill() {
        // Grok 只有 MCP 语义：装了 skill 即视为 Mcp 变体，未装为 None。
        return crate::integrations::grok_skill::is_installed().then_some(Variant::Mcp);
    }
    let text = std::fs::read_to_string(agent.file()).ok()?;
    if let Some(body) = block_body(&text) {
        return Some(classify_body(&body, agent));
    }
    if agent.is_owned_file() && is_managed_cursor_file(&text) {
        return Some(Variant::Cli);
    }
    None
}

/// 依据托管区块正文判定变体（纯函数，便于单测）。
///
/// 精确匹配当前内置正文优先；**漂移**（已装的是旧版本提示词、与当前正文不等）时改用结构性信号：
/// CLI 版必然指引「经 Shell/Bash 工具调用」，MCP 版只提工具调用、从不出现 `Shell/Bash`。
/// 这样即便内置提示词改版，已装规则仍能稳定归类，不会在更新后被错分模式。
pub fn classify_body(body: &str, agent: AgentTarget) -> Variant {
    if body == crate::prompts::mcp_reference_for(agent.kind()) {
        return Variant::Mcp;
    }
    if body == crate::prompts::cli_reference_for(agent.kind()) {
        return Variant::Cli;
    }
    if body.contains("Shell/Bash") {
        Variant::Cli
    } else {
        Variant::Mcp
    }
}

/// 当前平台是否支持（五家指令文件读写均跨平台）。
pub fn supported(_agent: AgentTarget) -> bool {
    true
}

/// 目标文件的展示路径（把 home 前缀折叠为 `~`）。
pub fn display_path(agent: AgentTarget) -> String {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::display_path();
    }
    collapse_home(&agent.file())
}

fn collapse_home(p: &Path) -> String {
    let home = paths::home();
    if let Ok(rest) = p.strip_prefix(&home) {
        format!("~/{}", rest.display())
    } else {
        p.display().to_string()
    }
}

// MARK: - 安装 / 卸载

/// 安装：写入最新推荐提示词（默认 CLI 变体，保持现状语义）。
pub fn install(agent: AgentTarget) -> Result<String> {
    install_variant(agent, Variant::Cli)
}

/// 更新：把已安装的旧提示词覆盖为最新（默认 CLI 变体）。
pub fn update(agent: AgentTarget) -> Result<String> {
    update_variant(agent, Variant::Cli)
}

/// 安装指定变体的提示词（托管区块 upsert，保留区块外用户内容）。Grok 委托 skill（忽略 variant）。
pub fn install_variant(agent: AgentTarget, variant: Variant) -> Result<String> {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::install();
    }
    write_rule(agent, &variant.body(agent))?;
    Ok(crate::i18n::tr(crate::i18n::Lang::current(), "cmd.ruleInstalled").to_string())
}

/// 更新为指定变体的最新提示词（与安装同样的写入逻辑，仅反馈文案不同）。Grok 委托 skill。
pub fn update_variant(agent: AgentTarget, variant: Variant) -> Result<String> {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::update();
    }
    write_rule(agent, &variant.body(agent))?;
    Ok(crate::i18n::tr(crate::i18n::Lang::current(), "cmd.ruleUpdated").to_string())
}

/// 把指定提示词正文写入目标文件（共享文件 upsert 区块 / Cursor 写 frontmatter + 区块、迁移旧格式）。
fn write_rule(agent: AgentTarget, body: &str) -> Result<()> {
    let path = agent.file();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("failed to create directory: {}", dir.display()))?;
    }
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    let new_text = if agent.is_owned_file() {
        build_cursor_rule(&old, body)
    } else {
        upsert_block(&old, body)
    };
    atomic_write(&path, new_text.as_bytes())
        .with_context(|| format!("failed to write rule file: {}", path.display()))?;
    Ok(())
}

/// 卸载：移除托管区块、保留区块外用户内容；Cursor 文件若区块外只剩 frontmatter / 空白则整文件删除。
pub fn uninstall(agent: AgentTarget) -> Result<String> {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::uninstall();
    }
    let path = agent.file();
    let lang = crate::i18n::Lang::current();
    if let Ok(old) = std::fs::read_to_string(&path) {
        // Cursor 旧格式独占整文件（头标记、无区块）：直接删除。
        if agent.is_owned_file() && is_managed_cursor_file(&old) && !has_block(&old) {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove rule file: {}", path.display()))?;
        } else if has_block(&old) {
            let new_text = remove_block(&old);
            if agent.is_owned_file() && cursor_residual_is_empty(&new_text) {
                std::fs::remove_file(&path)
                    .with_context(|| format!("failed to remove rule file: {}", path.display()))?;
            } else {
                atomic_write(&path, new_text.as_bytes())
                    .with_context(|| format!("failed to write rule file: {}", path.display()))?;
            }
        }
    }
    Ok(crate::i18n::tr(lang, "cmd.ruleRemoved").to_string())
}

/// 在文件管理器中定位规则文件。
pub fn reveal(agent: AgentTarget) {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::reveal();
    }
    let path = agent.file();
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .args(["-R", &path.to_string_lossy()])
            .spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let dir = path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| path.clone());
        let _ = std::process::Command::new("xdg-open").arg(dir).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer")
            .arg(format!("/select,{}", path.to_string_lossy()))
            .spawn();
    }
}

/// 用系统默认程序打开规则文件。
pub fn open(agent: AgentTarget) {
    if agent.is_grok_skill() {
        return crate::integrations::grok_skill::open();
    }
    let path = agent.file();
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(&path)
            .spawn();
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    }
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = "RULE BODY LINE 1\nRULE BODY LINE 2";

    #[test]
    fn parse_known_and_unknown() {
        assert_eq!(AgentTarget::parse("cursor"), Some(AgentTarget::Cursor));
        assert_eq!(AgentTarget::parse("claude"), Some(AgentTarget::ClaudeCode));
        assert_eq!(AgentTarget::parse("codex"), Some(AgentTarget::Codex));
        assert_eq!(AgentTarget::parse("grok"), Some(AgentTarget::Grok));
        assert_eq!(AgentTarget::parse("other"), None);
    }

    #[test]
    fn upsert_into_empty() {
        let out = upsert_block("", BODY);
        assert!(out.starts_with(BLOCK_BEGIN));
        assert!(out.contains(BODY));
        assert!(out.trim_end().ends_with(BLOCK_END));
        assert!(has_block(&out));
    }

    #[test]
    fn upsert_appends_and_preserves_user_content() {
        let user = "# My CLAUDE.md\n\nsome personal rules\n";
        let out = upsert_block(user, BODY);
        assert!(out.contains("some personal rules"));
        assert!(out.contains(BLOCK_BEGIN));
        // user content stays before the block
        assert!(out.find("some personal rules").unwrap() < out.find(BLOCK_BEGIN).unwrap());
    }

    #[test]
    fn upsert_is_idempotent_and_replaces_inner() {
        let once = upsert_block("keep me\n", BODY);
        let twice = upsert_block(&once, "NEW BODY");
        assert_eq!(twice.matches(BLOCK_BEGIN).count(), 1, "no duplicate block");
        assert_eq!(twice.matches(BLOCK_END).count(), 1);
        assert!(twice.contains("NEW BODY"));
        assert!(!twice.contains("RULE BODY LINE 1"));
        assert!(twice.contains("keep me"));
    }

    #[test]
    fn remove_only_block_keeps_rest() {
        let user = "# Title\n\nkeep this\n";
        let with = upsert_block(user, BODY);
        let without = remove_block(&with);
        assert!(!has_block(&without));
        assert!(without.contains("keep this"));
        assert!(without.contains("# Title"));
    }

    #[test]
    fn remove_from_empty_block_yields_empty() {
        let only = upsert_block("", BODY);
        let out = remove_block(&only);
        assert!(
            out.is_empty(),
            "removing the sole block clears the file: {out:?}"
        );
    }

    #[test]
    fn remove_noop_when_absent() {
        let user = "no block here\n";
        assert_eq!(remove_block(user), user);
    }

    #[test]
    fn block_body_extracts_inner() {
        let with = upsert_block("keep me\n", BODY);
        assert_eq!(block_body(&with).as_deref(), Some(BODY));
        assert_eq!(block_body("no block"), None);
    }

    #[test]
    fn cursor_fresh_install_has_frontmatter_and_block() {
        let f = build_cursor_rule("", BODY);
        assert!(f.starts_with("---\nalwaysApply: true\n---\n"));
        assert!(has_block(&f));
        assert_eq!(block_body(&f).as_deref(), Some(BODY));
        // new format no longer writes the legacy file marker
        assert!(!is_managed_cursor_file(&f));
    }

    #[test]
    fn cursor_preserves_user_content_and_updates_block() {
        let installed = build_cursor_rule("", BODY);
        let edited = format!("{installed}\nmy own cursor rule\n");
        let updated = build_cursor_rule(&edited, "NEW BODY");
        assert!(updated.contains("my own cursor rule"), "user content kept");
        assert_eq!(block_body(&updated).as_deref(), Some("NEW BODY"));
        assert_eq!(updated.matches(BLOCK_BEGIN).count(), 1, "single block");
        assert!(updated.contains("alwaysApply: true"), "frontmatter kept");
    }

    #[test]
    fn cursor_migrates_legacy_owned_file() {
        // 旧格式：frontmatter + 头标记 + 正文（无区块）。
        let legacy = format!("---\nalwaysApply: true\n---\n{MANAGED_FILE_MARK}\n\nOLD BODY\n");
        // 旧格式：被识别为已安装（owned + 头标记），但无区块 → 需迁移更新。
        assert!(is_managed_cursor_file(&legacy) && !has_block(&legacy));
        let migrated = build_cursor_rule(&legacy, BODY);
        assert!(has_block(&migrated));
        assert_eq!(block_body(&migrated).as_deref(), Some(BODY));
        assert!(!migrated.contains("OLD BODY"), "legacy body replaced");
        assert!(!is_managed_cursor_file(&migrated), "legacy marker dropped");
    }

    #[test]
    fn cursor_residual_empty_only_frontmatter() {
        let installed = build_cursor_rule("", BODY);
        let residual = remove_block(&installed);
        assert!(cursor_residual_is_empty(&residual), "only frontmatter left");
        let with_user = format!(
            "{}\nkeep this\n",
            remove_block(&build_cursor_rule("", BODY))
        );
        assert!(!cursor_residual_is_empty(&with_user));
    }

    #[test]
    fn classify_body_exact_and_drift() {
        // 精确匹配当前内置正文。
        assert_eq!(
            classify_body(
                &crate::prompts::cli_reference_for(crate::agents::AgentKind::Codex),
                AgentTarget::Codex,
            ),
            Variant::Cli
        );
        assert_eq!(
            classify_body(
                &crate::prompts::mcp_reference_for(crate::agents::AgentKind::Codex),
                AgentTarget::Codex,
            ),
            Variant::Mcp
        );
        // 漂移（旧版本提示词）：CLI 必含 Shell/Bash 指引 → Cli；MCP 从不提 Shell → Mcp。
        assert_eq!(
            classify_body(
                "... invoke via the Shell/Bash tool ... (older wording)",
                AgentTarget::Codex,
            ),
            Variant::Cli
        );
        assert_eq!(
            classify_body(
                "... call the AskHuman `ask` tool ... (older wording)",
                AgentTarget::Codex,
            ),
            Variant::Mcp
        );
    }

    #[test]
    fn variant_body_uses_the_same_subagent_scope_for_every_agent() {
        for variant in [Variant::Cli, Variant::Mcp] {
            for target in [
                AgentTarget::Codex,
                AgentTarget::ClaudeCode,
                AgentTarget::Cursor,
                AgentTarget::Grok,
            ] {
                let body = variant.body(target);
                assert!(body.contains(crate::prompts::SUBAGENT_PROTOCOL_RULE));
                assert!(!body.contains("task-suggestion generators"));
            }
        }
    }

    #[test]
    fn strip_frontmatter_removes_block() {
        assert_eq!(
            strip_frontmatter("---\nalwaysApply: true\n---\n").trim(),
            ""
        );
        assert_eq!(strip_frontmatter("---\nk: v\n---\nbody").trim(), "body");
        assert_eq!(strip_frontmatter("no fm").trim(), "no fm");
    }
}
