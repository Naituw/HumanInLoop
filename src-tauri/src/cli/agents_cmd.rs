//! `AskHuman agents <monitor|show|install|uninstall|update|help>` —— Agent 实时状态 + 集成。
//! 解决与原 `agents status`（GUI 窗口）命名冲突：状态窗口改名 `monitor`（增文本 / `--json`），
//! 集成动词 install/uninstall/update/show 复用 `integrations::{agent_rules,cursor_hook,claude_hook,agent_lifecycle}`。

use super::cfgio;
use crate::agents::AgentKind;
use crate::i18n::{err_prefix, Lang};
use crate::integrations::agent_rules::AgentTarget;
use crate::integrations::{
    agent_lifecycle, agent_mode, agent_permission, agent_rules, agent_stop, claude_hook,
    cursor_hook, mcp_config,
};
use serde_json::Value;
use std::process::exit;

const AGENTS: [&str; 5] = ["cursor", "claude", "codex", "grok", "pi"];

pub fn dispatch(args: &[String], lang: Lang) {
    // 无子命令 → 打印 help（与 channel/config 一致；不再默认开状态窗口）。
    let sub = args.first().map(|s| s.as_str()).unwrap_or("help");
    let rest = &args[args.len().min(1)..];
    let r = match sub {
        "monitor" => monitor(rest, lang),
        "mode" => mode_cmd(rest, lang),
        "update" => update_cmd(rest, lang),
        "permission" => permission_cmd(rest, lang),
        "stop" => stop_cmd(rest, lang),
        "lifecycle" => lifecycle_cmd(rest, lang),
        "cleanup" => cleanup_cmd(rest, lang),
        "install" | "uninstall" => Err(legacy_write_error(sub, lang)),
        "show" => show(rest, lang),
        "help" | "-h" | "--help" => {
            print_line(&help(lang));
            Ok(())
        }
        other => Err(cfgio::t(
            lang,
            &format!("unknown subcommand: {other}\n\n{}", help(lang)),
            &format!("未知子命令: {other}\n\n{}", help(lang)),
        )),
    };
    if let Err(e) = r {
        eprintln!("{}{}", err_prefix(lang), e);
        exit(1);
    }
}

// ——— monitor（状态）———

fn monitor(args: &[String], lang: Lang) -> Result<(), String> {
    let json = args.iter().any(|a| a == "--json");
    let text = args.iter().any(|a| a == "--text");

    if !json && !text && gui_available() {
        if crate::gui_host::host_open(crate::gui_host::WindowKind::Agents, false, None, None, None)
            .is_ok()
        {
            exit(0);
        }
        crate::app::run_agents(crate::config::AppConfig::load_without_secrets());
    }
    match cfgio::block_on(crate::client::request_agents_snapshot()) {
        Some(v) if json => {
            print_line(&serde_json::to_string_pretty(&v).unwrap_or_default());
            Ok(())
        }
        Some(v) => {
            print_line(&render_text(&v, lang));
            Ok(())
        }
        None => Err(cfgio::t(lang, "daemon not running", "daemon 未运行")),
    }
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
fn gui_available() -> bool {
    true
}
#[cfg(all(unix, not(target_os = "macos")))]
fn gui_available() -> bool {
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
}
#[cfg(not(any(unix, windows)))]
fn gui_available() -> bool {
    false
}

/// 把快照（AgentRecord 数组）渲染为分组文本：工作中 / 空闲 / 已结束。
fn render_text(snapshot: &Value, lang: Lang) -> String {
    let empty = vec![];
    let list = snapshot.as_array().unwrap_or(&empty);
    if list.is_empty() {
        return cfgio::t(lang, "No agents tracked.", "暂无被追踪的 agent。");
    }
    let now = now_secs();
    let mut out = String::new();
    for (state, title) in [
        ("working", cfgio::t(lang, "Working", "工作中")),
        ("idle", cfgio::t(lang, "Idle", "空闲")),
        ("ended", cfgio::t(lang, "Ended", "已结束")),
    ] {
        let group: Vec<&Value> = list
            .iter()
            .filter(|r| r.get("state").and_then(|s| s.as_str()) == Some(state))
            .collect();
        if group.is_empty() {
            continue;
        }
        out.push_str(&format!("{title} ({})\n", group.len()));
        for r in group {
            out.push_str(&format!("  {}\n", render_record(r, now, lang)));
        }
    }
    out.trim_end().to_string()
}

fn render_record(r: &Value, now: u64, lang: Lang) -> String {
    let kind = r.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
    let kind_label = AgentKind::parse(kind).map(|k| k.label()).unwrap_or(kind);
    let title = r
        .get("title")
        .and_then(|t| t.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cfgio::t(lang, "(untitled)", "（无标题）"));
    let sid = r.get("sessionId").and_then(|s| s.as_str()).unwrap_or("");
    let short = sid.chars().take(8).collect::<String>();
    let last = r.get("lastActivity").and_then(|v| v.as_u64()).unwrap_or(0);
    format!(
        "{kind_label} — {title}  ({}{}, {})",
        cfgio::t(lang, "session ", "会话 "),
        short,
        rel_time(now, last, lang)
    )
}

fn rel_time(now: u64, ts: u64, lang: Lang) -> String {
    if ts == 0 {
        return cfgio::t(lang, "unknown", "未知");
    }
    let d = now.saturating_sub(ts);
    if d < 60 {
        cfgio::t(lang, &format!("{d}s ago"), &format!("{d} 秒前"))
    } else if d < 3600 {
        let m = d / 60;
        cfgio::t(lang, &format!("{m}m ago"), &format!("{m} 分钟前"))
    } else if d < 86400 {
        let h = d / 3600;
        cfgio::t(lang, &format!("{h}h ago"), &format!("{h} 小时前"))
    } else {
        let days = d / 86400;
        cfgio::t(lang, &format!("{days}d ago"), &format!("{days} 天前"))
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ——— mode（三态编排：none | cli | mcp，与设置页同源）———

fn mode_cmd(args: &[String], lang: Lang) -> Result<(), String> {
    let agent = args.first().ok_or_else(|| {
        cfgio::t(
            lang,
            "usage: agents mode <agent> [none|cli|mcp]  (omit mode to query)",
            "用法: agents mode <agent> [none|cli|mcp]（省略模式则查询）",
        )
    })?;
    let target = AgentTarget::parse(agent).ok_or_else(|| {
        cfgio::t(
            lang,
            &format!("unknown agent: {agent} (expected cursor|claude|codex|grok|pi)"),
            &format!("未知 agent: {agent}（应为 cursor|claude|codex|grok|pi）"),
        )
    })?;
    let kind = AgentKind::parse(agent).unwrap();

    match args.get(1) {
        // 仅查询：当前模式 + 是否需更新。
        None => {
            let upd = if agent_mode::needs_update(target) {
                cfgio::t(lang, " (needs update)", "（需更新）")
            } else {
                String::new()
            };
            print_line(&format!(
                "[{}] {}{}",
                kind.label(),
                mode_label(agent_mode::current(target), lang),
                upd
            ));
            Ok(())
        }
        // 切换：先卸非目标产物，再装目标产物（底层幂等）。
        Some(want) => {
            let mode = agent_mode::Mode::parse(want).ok_or_else(|| {
                cfgio::t(
                    lang,
                    &format!("unknown mode: {want} (expected none|cli|mcp)"),
                    &format!("未知模式: {want}（应为 none|cli|mcp）"),
                )
            })?;
            agent_mode::set(target, mode).map_err(|e| e.to_string())?;
            print_line(&format!(
                "[{}] {} {}",
                kind.label(),
                cfgio::t(lang, "mode set to", "模式已设为"),
                mode_label(mode, lang)
            ));
            Ok(())
        }
    }
}

fn mode_label(m: agent_mode::Mode, lang: Lang) -> String {
    match m {
        agent_mode::Mode::None => cfgio::t(lang, "off", "未集成"),
        agent_mode::Mode::Cli => "CLI".to_string(),
        agent_mode::Mode::Mcp => "MCP".to_string(),
    }
}

// ——— 整包更新与独立 capability ———

fn parse_target(agent: &str, lang: Lang) -> Result<AgentTarget, String> {
    AgentTarget::parse(agent).ok_or_else(|| {
        cfgio::t(
            lang,
            &format!("unknown agent: {agent} (expected cursor|claude|codex|grok|pi)"),
            &format!("未知 agent: {agent}（应为 cursor|claude|codex|grok|pi）"),
        )
    })
}

fn update_cmd(args: &[String], lang: Lang) -> Result<(), String> {
    if args.len() > 1 || args.first().is_some_and(|arg| arg.starts_with('-')) {
        return Err(cfgio::t(
            lang,
            "usage: agents update [<agent>] (artifact flags are no longer supported)",
            "用法: agents update [<agent>]（不再支持产物 flags）",
        ));
    }
    let targets: Vec<(String, AgentTarget)> = if let Some(agent) = args.first() {
        vec![(agent.clone(), parse_target(agent, lang)?)]
    } else {
        AGENTS
            .iter()
            .filter_map(|agent| {
                let target = AgentTarget::parse(agent)?;
                (agent_mode::current(target) != agent_mode::Mode::None
                    || agent_mode::needs_update(target))
                .then(|| ((*agent).to_string(), target))
            })
            .collect()
    };
    let mut failures = Vec::new();
    for (name, target) in targets {
        match agent_mode::update(target) {
            Ok(()) => print_line(&format!("[{name}] {}", cfgio::t(lang, "updated", "已更新"))),
            Err(error) => {
                print_line(&format!(
                    "[{name}] {}{error}",
                    cfgio::t(lang, "error: ", "错误: ")
                ));
                failures.push(name);
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(cfgio::t(
            lang,
            &format!("failed to update: {}", failures.join(", ")),
            &format!("更新失败: {}", failures.join("、")),
        ))
    }
}

fn permission_cmd(args: &[String], lang: Lang) -> Result<(), String> {
    let agent = args.first().ok_or_else(|| {
        cfgio::t(
            lang,
            "usage: agents permission <claude|codex> [on|off]",
            "用法: agents permission <claude|codex> [on|off]",
        )
    })?;
    if args.len() > 2 || !matches!(agent.as_str(), "claude" | "codex") {
        return Err(cfgio::t(
            lang,
            "permission approval is supported only for claude and codex",
            "权限审批仅支持 claude 与 codex",
        ));
    }
    let target = parse_target(agent, lang)?;
    if let Some(value) = args.get(1) {
        let enabled = match value.as_str() {
            "on" => true,
            "off" => false,
            _ => return Err(cfgio::t(lang, "expected on or off", "应为 on 或 off")),
        };
        agent_permission::set_enabled(target, enabled).map_err(|e| e.to_string())?;
        print_line(&cfgio::t(
            lang,
            "This changes future permission prompts only; approvals already delivered by AskHuman remain valid.",
            "此设置只影响后续权限请求；AskHuman 已投递的在途审批仍然有效。",
        ));
    }
    let status = agent_permission::status(target);
    print_line(&format!(
        "[{agent}] {} — {}",
        if status.enabled { "on" } else { "off" },
        if status.installed {
            cfgio::t(lang, "configured", "已配置")
        } else {
            cfgio::t(lang, "not configured", "未配置")
        }
    ));
    Ok(())
}

fn stop_cmd(args: &[String], lang: Lang) -> Result<(), String> {
    let agent = args.first().ok_or_else(|| {
        cfgio::t(
            lang,
            "usage: agents stop <claude|codex|cursor> [on|off]",
            "用法: agents stop <claude|codex|cursor> [on|off]",
        )
    })?;
    if args.len() > 2 {
        return Err(cfgio::t(lang, "too many arguments", "参数过多"));
    }
    let kind =
        AgentKind::parse(agent).ok_or_else(|| cfgio::t(lang, "unknown agent", "未知 agent"))?;
    if !agent_stop::supported(kind) {
        return Err(cfgio::t(
            lang,
            "Stop confirmation is supported only for claude, codex, and cursor",
            "结束确认仅支持 claude、codex 与 cursor",
        ));
    }
    if let Some(value) = args.get(1) {
        let enabled = match value.as_str() {
            "on" => true,
            "off" => false,
            _ => return Err(cfgio::t(lang, "expected on or off", "应为 on 或 off")),
        };
        agent_stop::set_enabled(kind, enabled).map_err(|error| error.to_string())?;
    }
    let state = agent_stop::status(kind);
    print_line(&format!(
        "[{agent}] {}{}",
        if state.enabled { "on" } else { "off" },
        if state.outdated {
            cfgio::t(lang, " (needs update)", "（需更新）")
        } else {
            String::new()
        }
    ));
    Ok(())
}

fn lifecycle_cmd(args: &[String], lang: Lang) -> Result<(), String> {
    let agent = args.first().ok_or_else(|| {
        cfgio::t(
            lang,
            "usage: agents lifecycle <agent> [on|off]",
            "用法: agents lifecycle <agent> [on|off]",
        )
    })?;
    if args.len() > 2 {
        return Err(cfgio::t(lang, "too many arguments", "参数过多"));
    }
    let kind =
        AgentKind::parse(agent).ok_or_else(|| cfgio::t(lang, "unknown agent", "未知 agent"))?;
    if let Some(value) = args.get(1) {
        match value.as_str() {
            "on" => agent_lifecycle::install(kind).map_err(|e| e.to_string())?,
            "off" => agent_lifecycle::uninstall(kind).map_err(|e| e.to_string())?,
            _ => return Err(cfgio::t(lang, "expected on or off", "应为 on 或 off")),
        };
    }
    let status = agent_lifecycle::status(kind);
    print_line(&format!(
        "[{agent}] {}; {}{}",
        if status.enabled { "on" } else { "off" },
        if status.installed {
            cfgio::t(lang, "configured", "已配置")
        } else {
            cfgio::t(lang, "not configured", "未配置")
        },
        if status.needs_update {
            cfgio::t(lang, " (needs update)", "（需更新）")
        } else {
            String::new()
        }
    ));
    Ok(())
}

fn cleanup_cmd(args: &[String], lang: Lang) -> Result<(), String> {
    if !args.is_empty() {
        return Err(cfgio::t(
            lang,
            "usage: agents cleanup",
            "用法: agents cleanup",
        ));
    }

    let mut errors = Vec::new();
    for (target, kind) in [
        (AgentTarget::Cursor, AgentKind::Cursor),
        (AgentTarget::ClaudeCode, AgentKind::Claude),
        (AgentTarget::Codex, AgentKind::Codex),
        (AgentTarget::Grok, AgentKind::Grok),
        (AgentTarget::Pi, AgentKind::Pi),
    ] {
        if let Err(error) = agent_mode::set(target, agent_mode::Mode::None) {
            errors.push(format!("{} integration: {error}", kind.label()));
        }
    }
    if errors.is_empty() {
        print_line(&cfgio::t(
            lang,
            "Removed all AskHuman-managed Agent integrations.",
            "已移除全部由 AskHuman 托管的 Agent 集成。",
        ));
        Ok(())
    } else {
        Err(errors.join("\n"))
    }
}

fn legacy_write_error(command: &str, lang: Lang) -> String {
    cfgio::t(
        lang,
        &format!("agents {command} was removed; use `agents mode <agent> <cli|mcp|none>`, `agents update [agent]`, or an integration capability command"),
        &format!("agents {command} 已移除；请改用 `agents mode <agent> <cli|mcp|none>`、`agents update [agent]` 或集成内 capability 命令"),
    )
}

// ——— show（手动集成 + 状态）———

fn show(args: &[String], lang: Lang) -> Result<(), String> {
    let targets: Vec<&str> = match args.first().map(|s| s.as_str()) {
        Some(a) if !a.starts_with('-') => {
            if AgentTarget::parse(a).is_none() {
                return Err(cfgio::t(
                    lang,
                    &format!("unknown agent: {a}"),
                    &format!("未知 agent: {a}"),
                ));
            }
            vec![a]
        }
        _ => AGENTS.to_vec(),
    };

    // Show the target-specific body for a single agent. The all-agent view keeps the generic
    // reference because one shared block cannot represent Codex and non-Codex scope rules.
    if targets == ["grok"] {
        print_line(&crate::prompts::grok_skill_body());
    } else if targets.len() == 1 {
        let kind = AgentKind::parse(targets[0]).unwrap();
        print_line(&crate::prompts::cli_reference_for(kind));
    } else {
        print_line(&crate::prompts::cli_reference());
    }
    print_line("");
    let yes = cfgio::t(lang, "installed", "已安装");
    let no = cfgio::t(lang, "not installed", "未安装");
    let upd = cfgio::t(lang, " (needs update)", "（需更新）");
    let na = cfgio::t(lang, "n/a", "不适用");

    for name in targets {
        let target = AgentTarget::parse(name).unwrap();
        let kind = AgentKind::parse(name).unwrap();
        print_line(&format!("[{}]", kind.label()));

        // 当前模式（三态聚合）
        print_line(&format!(
            "  {}: {}",
            cfgio::t(lang, "mode", "模式"),
            mode_label(agent_mode::current(target), lang)
        ));

        // Rules
        let rules = if agent_rules::is_installed(target) {
            format!(
                "{yes}{}",
                if agent_rules::needs_update(target) {
                    upd.clone()
                } else {
                    String::new()
                }
            )
        } else {
            no.clone()
        };
        // Grok 的指令载体是 skill（非 rules 文件），标签相应改为 skill。
        let rules_label = if matches!(target, AgentTarget::Grok) {
            cfgio::t(lang, "skill", "skill")
        } else {
            cfgio::t(lang, "rules", "规则")
        };
        print_line(&format!(
            "  {}: {} — {}",
            rules_label,
            rules,
            agent_rules::display_path(target)
        ));

        // Hook
        let hook = match target {
            AgentTarget::Cursor => hook_state(
                cursor_hook::is_installed(),
                cursor_hook::needs_update(),
                &yes,
                &no,
                &upd,
            ),
            AgentTarget::ClaudeCode => hook_state(
                claude_hook::is_installed(),
                claude_hook::needs_update(),
                &yes,
                &no,
                &upd,
            ),
            AgentTarget::Codex | AgentTarget::Grok => na.clone(),
            AgentTarget::Pi => hook_state(
                agent_mode::timeout_hook_is_installed(target),
                agent_mode::timeout_hook_needs_update(target),
                &yes,
                &no,
                &upd,
            ),
        };
        let hook_label = if target == AgentTarget::Pi {
            cfgio::t(lang, "runtime extension", "运行时 Extension")
        } else {
            cfgio::t(lang, "timeout hook", "超时 hook")
        };
        print_line(&format!("  {}: {}", hook_label, hook));

        let permission = agent_permission::status(target);
        let permission_text = if !permission.supported {
            na.clone()
        } else {
            format!(
                "{}; {}{}",
                if permission.enabled { "on" } else { "off" },
                if permission.installed { &yes } else { &no },
                if permission.outdated { &upd } else { "" }
            )
        };
        print_line(&format!(
            "  {}: {}",
            cfgio::t(lang, "permission approval", "权限审批"),
            permission_text
        ));

        let stop = agent_stop::status(kind);
        let stop_text = if !stop.supported {
            na.clone()
        } else {
            format!(
                "{}; {}{}",
                if stop.enabled { "on" } else { "off" },
                if stop.installed { &yes } else { &no },
                if stop.outdated { &upd } else { "" }
            )
        };
        print_line(&format!(
            "  {}: {}",
            cfgio::t(lang, "Stop confirmation", "结束确认"),
            stop_text
        ));

        // MCP 配置（用户级全局）
        let mcp = if !mcp_config::supported(target) {
            na.clone()
        } else if mcp_config::is_installed(target) {
            format!(
                "{yes}{}",
                if mcp_config::needs_update(target) {
                    upd.clone()
                } else {
                    String::new()
                }
            )
        } else {
            no.clone()
        };
        if mcp_config::supported(target) {
            print_line(&format!(
                "  {}: {} — {}",
                cfgio::t(lang, "mcp config", "MCP 配置"),
                mcp,
                mcp_config::display_path(target)
            ));
        } else {
            print_line(&format!(
                "  {}: {}",
                cfgio::t(lang, "mcp config", "MCP 配置"),
                mcp
            ));
        }

        // Lifecycle capability owned by the active automatic integration.
        let st = agent_lifecycle::status(kind);
        let lc = if !st.supported {
            na.clone()
        } else {
            format!(
                "{}; {}{}",
                if st.enabled { "on" } else { "off" },
                if st.installed { &yes } else { &no },
                if st.needs_update {
                    upd.clone()
                } else {
                    String::new()
                }
            )
        };
        print_line(&format!(
            "  {}: {}",
            cfgio::t(lang, "lifecycle tracking", "生命周期追踪"),
            lc
        ));
        print_line("");
    }
    Ok(())
}

fn hook_state(installed: bool, needs_update: bool, yes: &str, no: &str, upd: &str) -> String {
    if installed {
        format!("{yes}{}", if needs_update { upd } else { "" })
    } else {
        no.to_string()
    }
}

fn help(lang: Lang) -> String {
    cfgio::t(
        lang,
        "AskHuman agents — agent status + integrations (cursor | claude | codex | grok | pi)\n\
\n\
  agents monitor [--json|--text]     Live agent status (opens a window when a GUI is available)\n\
  agents mode <agent> [none|cli|mcp] Switch the integration mode (omit to query); auto-swaps products\n\
  agents update [<agent>]            Refresh each current mode's complete managed bundle\n\
  agents permission <claude|codex> [on|off]  Query or set permission approval\n\
  agents stop <claude|codex|cursor|pi> [on|off]  Query or set Stop confirmation\n\
  agents lifecycle <agent> [on|off]  Query or set lifecycle tracking inside an active integration\n\
  agents cleanup                     Remove every AskHuman-managed Agent artifact\n\
  agents show [<agent>]              Manual-integration prompt + paste paths + install status\n\
\n\
  Modes: cli = rules + runtime + default-on lifecycle; mcp = rules/skill + MCP + default-on lifecycle; none = remove.\n\
  Grok only supports none | mcp (skill + MCP config); it has no CLI mode and no timeout hook.\n\
  Pi only supports none | cli; its runtime artifact is a managed Extension and Stop defaults on.\n\
  Legacy install/uninstall and per-artifact write flags have been removed.",
        "AskHuman agents —— agent 状态 + 集成（cursor | claude | codex | grok | pi）\n\
\n\
  agents monitor [--json|--text]     实时 agent 状态（有 GUI 时开窗）\n\
  agents mode <agent> [none|cli|mcp] 切换集成模式（省略则查询）；自动切换底层产物\n\
  agents update [<agent>]            更新当前模式的完整托管产物包\n\
  agents permission <claude|codex> [on|off]  查询或设置权限审批\n\
  agents stop <claude|codex|cursor|pi> [on|off]  查询或设置结束确认\n\
  agents lifecycle <agent> [on|off]  查询或设置已启用集成内的生命周期追踪\n\
  agents cleanup                     移除全部由 AskHuman 托管的 Agent 产物\n\
  agents show [<agent>]              手动集成提示词 + 粘贴位置 + 安装状态\n\
\n\
  模式: cli = 规则 + runtime + 默认开启的生命周期；mcp = 规则/skill + MCP + 默认开启的生命周期；none = 移除。\n\
  Grok 仅支持 none | mcp（skill + MCP 配置）；无 CLI 模式、无超时 hook。\n\
  Pi 仅支持 none | cli；运行时产物为托管 Extension，Stop 默认开启。\n\
  旧 install/uninstall 与逐产物写 flags 已移除。",
    )
}

fn print_line(s: &str) {
    super::print_line(s);
}
