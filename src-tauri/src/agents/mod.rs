//! Agent lifecycle tracking.
//!
//! Claude Code / Codex / Cursor / Grok 通过用户级 lifecycle hook，Pi 通过受管 Extension，把
//! `session-start` / `turn-start` / `turn-end` / `session-end` 事件经隐藏子命令
//! `AskHuman __agent-hook <agent> <event>` 上报给常驻 daemon；daemon 维护一张
//! agent 注册表（进程存活轮询 + TTL 兜底推导「工作中 / 空闲 / 已结束」），并把
//! 全量快照推送给 `AskHuman agents monitor` 打开的 GUI 状态窗口。
//!
//! 设计与决策见 `docs/specs/agent-lifecycle-tracking.md`，调研见
//! `demo/agent-lifecycle/FINDINGS.md`。

pub mod activity;
pub mod context_recovery;
pub mod cursor_vscdb;
pub mod detect;
pub mod interject;
pub mod registry;
pub mod report;
pub mod session_paths;
pub mod stop;
pub mod title;
pub mod transcript_full;
pub mod workspaces;

use serde::{Deserialize, Serialize};

/// 被追踪的 Agent 家族。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Codex,
    Cursor,
    Grok,
    Pi,
}

impl AgentKind {
    /// 线上协议 / 命令行使用的小写标识。
    pub fn as_str(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Codex => "codex",
            AgentKind::Cursor => "cursor",
            AgentKind::Grok => "grok",
            AgentKind::Pi => "pi",
        }
    }

    /// 展示名（窗口分组标题）。
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Codex => "Codex",
            AgentKind::Cursor => "Cursor",
            AgentKind::Grok => "Grok",
            AgentKind::Pi => "Pi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(AgentKind::Claude),
            "codex" => Some(AgentKind::Codex),
            "cursor" => Some(AgentKind::Cursor),
            "grok" => Some(AgentKind::Grok),
            "pi" => Some(AgentKind::Pi),
            _ => None,
        }
    }

    /// All supported agent families (for iteration).
    pub const ALL: [AgentKind; 5] = [
        AgentKind::Claude,
        AgentKind::Codex,
        AgentKind::Cursor,
        AgentKind::Grok,
        AgentKind::Pi,
    ];
}

/// 归一化后的生命周期事件（线上 `<event>` 取值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleEvent {
    SessionStart,
    TurnStart,
    TurnEnd,
    SessionEnd,
    /// 「仍在活动」：回合进行中的工具调用（Pre/PostToolUse）。刷新活动时间 + 保持/置为「工作中」，
    /// **不**结束回合。用于喂「Working 兜底超时」的活动心跳，避免长回合被误判空闲。
    Activity,
}

impl LifecycleEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            LifecycleEvent::SessionStart => "session-start",
            LifecycleEvent::TurnStart => "turn-start",
            LifecycleEvent::TurnEnd => "turn-end",
            LifecycleEvent::SessionEnd => "session-end",
            LifecycleEvent::Activity => "activity",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "session-start" => Some(LifecycleEvent::SessionStart),
            "turn-start" => Some(LifecycleEvent::TurnStart),
            "turn-end" => Some(LifecycleEvent::TurnEnd),
            "session-end" => Some(LifecycleEvent::SessionEnd),
            "activity" => Some(LifecycleEvent::Activity),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_roundtrip() {
        for k in AgentKind::ALL {
            assert_eq!(AgentKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(AgentKind::parse("CURSOR"), Some(AgentKind::Cursor));
        assert_eq!(AgentKind::parse("nope"), None);
    }

    #[test]
    fn event_roundtrip() {
        for e in [
            LifecycleEvent::SessionStart,
            LifecycleEvent::TurnStart,
            LifecycleEvent::TurnEnd,
            LifecycleEvent::SessionEnd,
            LifecycleEvent::Activity,
        ] {
            assert_eq!(LifecycleEvent::parse(e.as_str()), Some(e));
        }
        assert_eq!(LifecycleEvent::parse("bogus"), None);
    }
}
