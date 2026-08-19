//! 三态模式编排：把每家 Agent 的「Rule + Subagent Guard +（超时 Hook | MCP 配置）」聚合为
//! **None / Cli / Mcp** 三态互斥，
//! 供设置 UI 与 `agents`/`doctor` CLI 复用。
//!
//! - **Cli** 模式绑定：CLI 版 Rule + Guard + 超时 Hook（Codex 无超时 Hook）。
//! - **Mcp** 模式绑定：MCP 版 Rule + Guard + MCP 配置（用户级全局）。
//! - 一键切换（[`set`]）：先卸掉「非目标模式」的全部产物，再装目标模式产物；天然幂等。
//!
//! Lifecycle tracking is an optional capability owned by an active automatic integration. New
//! integrations adopt the default-on preference; explicit opt-out survives mode updates. None
//! mode always removes the on-disk lifecycle artifact.

use crate::integrations::agent_rules::{self, AgentTarget, Variant};
use crate::integrations::{
    agent_ask_question, agent_context_recovery, agent_lifecycle, agent_permission, agent_stop,
    agent_subagent_guard, claude_hook, cursor_hook, mcp_config, mutation_lock,
};
use anyhow::Result;

/// 每家 Agent 的集成模式（互斥三态）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    None,
    Cli,
    Mcp,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::None => "none",
            Mode::Cli => "cli",
            Mode::Mcp => "mcp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "none" => Some(Mode::None),
            "cli" => Some(Mode::Cli),
            "mcp" => Some(Mode::Mcp),
            _ => None,
        }
    }
}

// MARK: - 超时 Hook 分派（Codex 无超时 Hook）

/// Whether this agent has a managed timeout runtime artifact. Pi uses an Extension rather than a
/// native hook, but keeps this compatibility surface while the UI wire format is migrated.
pub fn timeout_hook_supported(target: AgentTarget) -> bool {
    match target {
        AgentTarget::Cursor => cursor_hook::supported(),
        AgentTarget::ClaudeCode => claude_hook::supported(),
        AgentTarget::Codex | AgentTarget::Grok => false,
        AgentTarget::Pi => true,
    }
}

/// 超时 Hook 是否已安装（Codex / Grok 恒 false）。
pub fn timeout_hook_is_installed(target: AgentTarget) -> bool {
    match target {
        AgentTarget::Cursor => cursor_hook::is_installed(),
        AgentTarget::ClaudeCode => claude_hook::is_installed(),
        AgentTarget::Codex | AgentTarget::Grok => false,
        AgentTarget::Pi => super::pi_extension::status().cli,
    }
}

/// 超时 Hook 是否需更新（Codex / Grok 恒 false）。
pub fn timeout_hook_needs_update(target: AgentTarget) -> bool {
    match target {
        AgentTarget::Cursor => cursor_hook::needs_update(),
        AgentTarget::ClaudeCode => claude_hook::needs_update(),
        AgentTarget::Codex | AgentTarget::Grok => false,
        AgentTarget::Pi => {
            let status = super::pi_extension::status();
            status.cli && status.outdated
        }
    }
}

fn timeout_hook_install(target: AgentTarget) -> Result<()> {
    match target {
        AgentTarget::Cursor => cursor_hook::install().map(|_| ()),
        AgentTarget::ClaudeCode => claude_hook::install().map(|_| ()),
        AgentTarget::Codex | AgentTarget::Grok => Ok(()),
        AgentTarget::Pi => super::pi_extension::set_cli_enabled(true),
    }
}

fn timeout_hook_uninstall(target: AgentTarget) -> Result<()> {
    match target {
        AgentTarget::Cursor => cursor_hook::uninstall().map(|_| ()),
        AgentTarget::ClaudeCode => claude_hook::uninstall().map(|_| ()),
        AgentTarget::Codex | AgentTarget::Grok => Ok(()),
        AgentTarget::Pi => super::pi_extension::set_cli_enabled(false),
    }
}

/// 在文件管理器中定位超时 Hook 的配置文件（Codex / Grok 无 Hook，no-op）。
pub fn timeout_hook_reveal(target: AgentTarget) {
    match target {
        AgentTarget::Cursor => cursor_hook::reveal(),
        AgentTarget::ClaudeCode => claude_hook::reveal(),
        AgentTarget::Codex | AgentTarget::Grok => {}
        AgentTarget::Pi => super::pi_extension::reveal(),
    }
}

/// 用系统默认程序打开超时 Hook 的配置文件（Codex / Grok 无 Hook，no-op）。
pub fn timeout_hook_open(target: AgentTarget) {
    match target {
        AgentTarget::Cursor => cursor_hook::open(),
        AgentTarget::ClaudeCode => claude_hook::open(),
        AgentTarget::Codex | AgentTarget::Grok => {}
        AgentTarget::Pi => super::pi_extension::open(),
    }
}

// MARK: - 状态

/// 当前模式：**以产物（MCP 配置 / 超时 Hook）为首要信号**，产物不明确时再回退到 Rule 正文变体。
///
/// 之所以产物优先：MCP 配置与超时 Hook 由 [`set`] 维护、彼此互斥，是稳定的模式标识；而 Rule 正文会随
/// 内置提示词版本演进而漂移，若以「正文是否精确等于当前 `mcp_reference()`」判定，一旦更新提示词，已装的
/// 旧正文就会失配并被错判成 CLI（曾导致「装了 MCP、改版后却显示 CLI 且提示需更新」的 bug）。
pub fn current(target: AgentTarget) -> Mode {
    let mcp = mcp_config::is_installed(target);
    let hook = timeout_hook_is_installed(target);
    current_from_artifacts(mcp, hook, agent_rules::installed_variant(target))
}

fn current_from_artifacts(mcp: bool, runtime: bool, rule: Option<Variant>) -> Mode {
    match (mcp, runtime) {
        (true, false) => Mode::Mcp,
        (false, true) => Mode::Cli,
        // Runtime artifacts are absent (for example Codex CLI) or conflict after a manual edit;
        // fall back to the managed Rule variant. Lifecycle is intentionally not an input.
        _ => match rule {
            Some(Variant::Mcp) => Mode::Mcp,
            Some(Variant::Cli) => Mode::Cli,
            None => Mode::None,
        },
    }
}

/// 当前模式下的某类产物（供「按产物」过期判断与单项更新）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Artifact {
    Rule,
    Hook,
    Mcp,
}

impl Artifact {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "rule" => Some(Artifact::Rule),
            "hook" => Some(Artifact::Hook),
            "mcp" => Some(Artifact::Mcp),
            _ => None,
        }
    }
}

/// 当前模式下各产物是否过期 / 缺失（与 [`needs_update`] 同口径，逐产物拆开供 UI 概览统计与单项更新）。
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct ArtifactUpdates {
    pub rule: bool,
    pub hook: bool,
    pub mcp: bool,
}

#[derive(Clone, Copy, Default)]
struct ArtifactState {
    rule_installed: bool,
    rule_outdated: bool,
    guard_outdated: bool,
    timeout_supported: bool,
    timeout_installed: bool,
    timeout_outdated: bool,
    permission_outdated: bool,
    recovery_outdated: bool,
    mcp_installed: bool,
    mcp_outdated: bool,
}

/// 逐产物计算当前模式下的过期 / 缺失情况。None 模式仅报告需要清理的残留 Permission Hook。
pub fn artifact_updates(target: AgentTarget) -> ArtifactUpdates {
    let mode = current(target);
    let updates = artifact_updates_for(
        mode,
        ArtifactState {
            rule_installed: agent_rules::is_installed(target),
            rule_outdated: match mode {
                Mode::Cli => agent_rules::needs_update_variant(target, Variant::Cli),
                Mode::Mcp => agent_rules::needs_update_variant(target, Variant::Mcp),
                Mode::None => false,
            },
            guard_outdated: agent_subagent_guard::needs_update(target, mode),
            timeout_supported: timeout_hook_supported(target),
            timeout_installed: timeout_hook_is_installed(target),
            timeout_outdated: timeout_hook_needs_update(target),
            permission_outdated: permission_needs_reconcile(target, mode),
            recovery_outdated: agent_context_recovery::needs_update(target, mode),
            mcp_installed: mcp_config::is_installed(target),
            mcp_outdated: mcp_config::needs_update(target),
        },
    );
    merge_lifecycle_update(
        updates,
        agent_lifecycle::status_for_mode(kind_for_target(target), mode).needs_update,
    )
}

fn merge_lifecycle_update(
    mut updates: ArtifactUpdates,
    lifecycle_needs_update: bool,
) -> ArtifactUpdates {
    updates.hook |= lifecycle_needs_update;
    updates
}

fn artifact_updates_for(mode: Mode, state: ArtifactState) -> ArtifactUpdates {
    match mode {
        Mode::None => ArtifactUpdates {
            hook: state.permission_outdated || state.recovery_outdated,
            ..ArtifactUpdates::default()
        },
        Mode::Cli => ArtifactUpdates {
            rule: !state.rule_installed || state.rule_outdated || state.guard_outdated,
            hook: (state.timeout_supported && (!state.timeout_installed || state.timeout_outdated))
                || state.permission_outdated
                || state.recovery_outdated,
            mcp: false,
        },
        Mode::Mcp => ArtifactUpdates {
            rule: !state.rule_installed || state.rule_outdated || state.guard_outdated,
            hook: state.permission_outdated,
            mcp: !state.mcp_installed || state.mcp_outdated || state.recovery_outdated,
        },
    }
}

fn permission_needs_reconcile(target: AgentTarget, _mode: Mode) -> bool {
    let status = agent_permission::status(target);
    if !status.supported {
        return false;
    }
    status.needs_update
}

/// 当前模式下是否有产物过期 / 缺失（含 Rule / Guard、超时 Hook 与 MCP 配置）。
pub fn needs_update(target: AgentTarget) -> bool {
    let u = artifact_updates(target);
    u.rule || u.hook || u.mcp
}

// MARK: - 切换

/// 一键设为目标模式并完整 reconcile 该模式的托管产物。重复设置同一 mode 也会更新磁盘；
/// Permission 是否安装只读取其独立 preference，本操作绝不改写该 preference。
///
/// Grok 只提供 `None | Mcp` 两态（Composer 的 CLI 会自动后台化、不可靠，见调研）：请求 `Cli` 直接报错，
/// 避免留下「装了 skill 却没 MCP 配置」的半残状态。Grok 的 `Mcp` 产物 = skill（经 `agent_rules` 委托）+
/// MCP 配置，正好复用下方 Mcp 分支（无超时 Hook）。
pub fn set(target: AgentTarget, mode: Mode) -> Result<()> {
    if target == AgentTarget::Grok && mode == Mode::Cli {
        return Err(anyhow::anyhow!(
            "Grok only supports None | Mcp (no CLI mode)"
        ));
    }
    if target == AgentTarget::Pi && mode == Mode::Mcp {
        return Err(anyhow::anyhow!("Pi only supports None | Cli (no MCP mode)"));
    }
    let _lock = mutation_lock::IntegrationMutationLock::acquire()?;
    set_unlocked(target, mode)
}

fn set_unlocked(target: AgentTarget, mode: Mode) -> Result<()> {
    match mode {
        Mode::None => uninstall_all_unlocked(target),
        Mode::Cli => {
            // 卸 MCP 产物 → 装 CLI Rule + 超时 Hook（Codex 跳过 Hook）。
            mcp_config::uninstall(target)?;
            agent_rules::install_variant(target, Variant::Cli)?;
            agent_subagent_guard::reconcile_unlocked(target, mode)?;
            if timeout_hook_supported(target) {
                timeout_hook_install(target)?;
            }
            agent_context_recovery::reconcile_unlocked(target, mode)?;
            agent_lifecycle::reconcile_unlocked(kind_for_target(target), mode, true)?;
            agent_permission::reconcile_unlocked(target, mode)?;
            agent_stop::reconcile_unlocked(kind_for_target(target), mode)?;
            agent_ask_question::reconcile_unlocked(kind_for_target(target), mode)?;
            Ok(())
        }
        Mode::Mcp => {
            // 卸超时 Hook → 装 MCP Rule + MCP 配置。
            if timeout_hook_supported(target) {
                timeout_hook_uninstall(target)?;
            }
            agent_rules::install_variant(target, Variant::Mcp)?;
            agent_subagent_guard::reconcile_unlocked(target, mode)?;
            mcp_config::install(target)?;
            agent_context_recovery::reconcile_unlocked(target, mode)?;
            agent_lifecycle::reconcile_unlocked(kind_for_target(target), mode, true)?;
            agent_permission::reconcile_unlocked(target, mode)?;
            agent_stop::reconcile_unlocked(kind_for_target(target), mode)?;
            agent_ask_question::reconcile_unlocked(kind_for_target(target), mode)?;
            Ok(())
        }
    }
}

/// 更新当前模式的全部产物到最新（不切换模式）。None 有残留时清理，clean None 幂等 no-op。
pub fn update(target: AgentTarget) -> Result<()> {
    set(target, current(target))
}

/// 把当前模式下的**单个产物**刷新到最新（不切换模式、不动其它产物）。各底层 install 均幂等，
/// 故「重装即更新」；与当前模式不相干的产物（如 None、或在 Cli 模式更新 Mcp）为 no-op。
pub fn update_artifact(target: AgentTarget, artifact: Artifact) -> Result<()> {
    let _lock = mutation_lock::IntegrationMutationLock::acquire()?;
    let mode = current(target);
    match (mode, artifact) {
        (Mode::Cli, Artifact::Rule) => {
            agent_rules::install_variant(target, Variant::Cli)?;
            agent_subagent_guard::reconcile_unlocked(target, mode)
        }
        (Mode::Mcp, Artifact::Rule) => {
            agent_rules::install_variant(target, Variant::Mcp)?;
            agent_subagent_guard::reconcile_unlocked(target, mode)
        }
        (Mode::Cli, Artifact::Hook) => {
            if timeout_hook_supported(target) {
                timeout_hook_install(target)?;
            }
            agent_context_recovery::reconcile_unlocked(target, mode)?;
            agent_lifecycle::reconcile_unlocked(kind_for_target(target), mode, true)?;
            agent_permission::reconcile_unlocked(target, mode)
        }
        (Mode::Mcp, Artifact::Hook) | (Mode::None, Artifact::Hook) => {
            agent_context_recovery::reconcile_unlocked(target, mode)?;
            agent_lifecycle::reconcile_unlocked(kind_for_target(target), mode, true)?;
            agent_permission::reconcile_unlocked(target, mode)
        }
        (Mode::Mcp, Artifact::Mcp) => {
            mcp_config::install(target)?;
            agent_context_recovery::reconcile_unlocked(target, mode)
        }
        _ => Ok(()),
    }
}

/// 卸载当前 / 全部模式产物（Rule + Guard + 超时 Hook + MCP 配置），保留用户其它内容。
fn uninstall_all_unlocked(target: AgentTarget) -> Result<()> {
    agent_context_recovery::reconcile_unlocked(target, Mode::None)?;
    agent_rules::uninstall(target)?;
    agent_subagent_guard::reconcile_unlocked(target, Mode::None)?;
    if timeout_hook_supported(target) {
        timeout_hook_uninstall(target)?;
    }
    mcp_config::uninstall(target)?;
    agent_lifecycle::reconcile_unlocked(kind_for_target(target), Mode::None, false)?;
    agent_permission::reconcile_unlocked(target, Mode::None)?;
    agent_stop::reconcile_unlocked(kind_for_target(target), Mode::None)?;
    agent_ask_question::reconcile_unlocked(kind_for_target(target), Mode::None)?;
    Ok(())
}

pub(crate) fn kind_for_target(target: AgentTarget) -> crate::agents::AgentKind {
    match target {
        AgentTarget::Cursor => crate::agents::AgentKind::Cursor,
        AgentTarget::ClaudeCode => crate::agents::AgentKind::Claude,
        AgentTarget::Codex => crate::agents::AgentKind::Codex,
        AgentTarget::Grok => crate::agents::AgentKind::Grok,
        AgentTarget::Pi => crate::agents::AgentKind::Pi,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_parse_as_str_roundtrip() {
        for m in [Mode::None, Mode::Cli, Mode::Mcp] {
            assert_eq!(Mode::parse(m.as_str()), Some(m));
        }
        assert_eq!(Mode::parse("other"), None);
    }

    #[test]
    fn mode_detection_uses_only_core_integration_artifacts() {
        assert_eq!(current_from_artifacts(true, false, None), Mode::Mcp);
        assert_eq!(current_from_artifacts(false, true, None), Mode::Cli);
        assert_eq!(
            current_from_artifacts(false, false, Some(Variant::Cli)),
            Mode::Cli
        );
        assert_eq!(current_from_artifacts(false, false, None), Mode::None);
    }

    #[test]
    fn codex_has_no_timeout_hook() {
        assert!(!timeout_hook_supported(AgentTarget::Codex));
        assert!(!timeout_hook_is_installed(AgentTarget::Codex));
        assert!(!timeout_hook_needs_update(AgentTarget::Codex));
    }

    #[test]
    fn kind_for_target_covers_every_agent_target() {
        assert_eq!(
            kind_for_target(AgentTarget::Cursor),
            crate::agents::AgentKind::Cursor
        );
        assert_eq!(
            kind_for_target(AgentTarget::ClaudeCode),
            crate::agents::AgentKind::Claude
        );
        assert_eq!(
            kind_for_target(AgentTarget::Codex),
            crate::agents::AgentKind::Codex
        );
        assert_eq!(
            kind_for_target(AgentTarget::Grok),
            crate::agents::AgentKind::Grok
        );
        assert_eq!(
            kind_for_target(AgentTarget::Pi),
            crate::agents::AgentKind::Pi
        );
    }

    #[test]
    fn artifact_updates_route_recovery_to_the_mode_owned_artifact() {
        let clean = ArtifactState {
            rule_installed: true,
            timeout_installed: true,
            mcp_installed: true,
            ..ArtifactState::default()
        };
        assert_eq!(
            artifact_updates_for(Mode::None, clean),
            ArtifactUpdates::default()
        );
        assert_eq!(
            artifact_updates_for(
                Mode::None,
                ArtifactState {
                    recovery_outdated: true,
                    ..clean
                }
            ),
            ArtifactUpdates {
                hook: true,
                ..ArtifactUpdates::default()
            }
        );
        assert_eq!(
            artifact_updates_for(
                Mode::Cli,
                ArtifactState {
                    recovery_outdated: true,
                    ..clean
                }
            ),
            ArtifactUpdates {
                hook: true,
                ..ArtifactUpdates::default()
            }
        );
        assert_eq!(
            artifact_updates_for(
                Mode::Mcp,
                ArtifactState {
                    recovery_outdated: true,
                    ..clean
                }
            ),
            ArtifactUpdates {
                mcp: true,
                ..ArtifactUpdates::default()
            }
        );
    }

    #[test]
    fn artifact_updates_keep_rule_timeout_permission_and_mcp_independent() {
        let clean = ArtifactState {
            rule_installed: true,
            timeout_supported: true,
            timeout_installed: true,
            mcp_installed: true,
            ..ArtifactState::default()
        };
        assert_eq!(
            artifact_updates_for(Mode::Cli, clean),
            ArtifactUpdates::default()
        );
        assert_eq!(
            artifact_updates_for(Mode::Mcp, clean),
            ArtifactUpdates::default()
        );

        for state in [
            ArtifactState {
                rule_installed: false,
                ..clean
            },
            ArtifactState {
                rule_outdated: true,
                ..clean
            },
            ArtifactState {
                guard_outdated: true,
                ..clean
            },
        ] {
            assert!(artifact_updates_for(Mode::Cli, state).rule);
            assert!(artifact_updates_for(Mode::Mcp, state).rule);
        }
        for state in [
            ArtifactState {
                timeout_installed: false,
                ..clean
            },
            ArtifactState {
                timeout_outdated: true,
                ..clean
            },
            ArtifactState {
                permission_outdated: true,
                ..clean
            },
        ] {
            assert!(artifact_updates_for(Mode::Cli, state).hook);
        }
        assert!(
            !artifact_updates_for(
                Mode::Cli,
                ArtifactState {
                    timeout_supported: false,
                    timeout_installed: false,
                    ..clean
                }
            )
            .hook
        );
        assert!(
            artifact_updates_for(
                Mode::Mcp,
                ArtifactState {
                    permission_outdated: true,
                    ..clean
                }
            )
            .hook
        );
        for state in [
            ArtifactState {
                mcp_installed: false,
                ..clean
            },
            ArtifactState {
                mcp_outdated: true,
                ..clean
            },
        ] {
            assert!(artifact_updates_for(Mode::Mcp, state).mcp);
        }
    }

    #[test]
    fn lifecycle_drift_is_classified_as_a_hook_update() {
        let base = ArtifactUpdates {
            rule: true,
            hook: false,
            mcp: false,
        };
        assert_eq!(merge_lifecycle_update(base, false), base);
        assert_eq!(
            merge_lifecycle_update(base, true),
            ArtifactUpdates { hook: true, ..base }
        );
    }
}
