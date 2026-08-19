// Shared helpers for the history window (list + read-only detail).

import type {
  HistoryEntry,
  HistorySessionGroup,
  HistorySessionRef,
} from "./types";

/** Default caller source name (backend `models::DEFAULT_SOURCE_NAME`). */
export const DEFAULT_SOURCE_NAME = "the Loop";
export const ALL_HISTORY_SESSIONS = "__all_history_sessions__";

// Known agent-family display names (as recorded in `source` by older versions,
// identical in zh/en) → family id. Lets legacy entries without `agentKind`
// still resolve to an agent family.
const SOURCE_TO_KIND: Record<string, string> = {
  "claude code": "claude",
  codex: "codex",
  cursor: "cursor",
  grok: "grok",
  pi: "pi",
};

/**
 * Effective agent family of an entry: persisted `agentKind`, falling back to a
 * `source` that matches a known family display name (legacy entries).
 */
export function agentKindOf(e: HistoryEntry): string {
  if (e.agentKind) return e.agentKind;
  return SOURCE_TO_KIND[e.source.trim().toLowerCase()] ?? "";
}

/** Workspace display name (folder basename of the project root path). */
export function workspaceNameOf(e: HistoryEntry): string {
  if (!e.project) return "";
  const parts = e.project.replace(/[\\/]+$/, "").split(/[\\/]/);
  return parts[parts.length - 1] || e.project;
}

/**
 * Custom caller source name worth showing next to the agent badge: non-empty,
 * not the built-in default, and not just the agent family label itself.
 */
export function customSourceOf(e: HistoryEntry, agentLabel: string): string {
  const s = e.source.trim();
  if (!s || s === DEFAULT_SOURCE_NAME) return "";
  if (agentLabel && s.toLowerCase() === agentLabel.trim().toLowerCase()) return "";
  return s;
}

function nonempty(value: string | null | undefined): string {
  return value?.trim() ?? "";
}

/**
 * Return the strongest trustworthy session partition stored on an entry.
 * Legacy source-name inference is deliberately not used for identity.
 */
export function historySessionOf(e: HistoryEntry): HistorySessionRef {
  const agentKind = nonempty(e.agentKind);
  const sessionId = nonempty(e.agentSessionId);
  if (agentKind && sessionId) {
    return { type: "agent", agentKind, sessionId };
  }
  const project = nonempty(e.project);
  const instanceId = nonempty(e.mcpInstanceId);
  if (project && instanceId) {
    return { type: "mcp", project, instanceId };
  }
  return { type: "unbound" };
}

/** Stable collision-free token for a structured history session partition. */
export function historySessionToken(ref: HistorySessionRef): string {
  switch (ref.type) {
    case "agent":
      return JSON.stringify(["agent", ref.agentKind, ref.sessionId]);
    case "mcp":
      return JSON.stringify(["mcp", ref.project, ref.instanceId]);
    case "unbound":
      return JSON.stringify(["unbound"]);
  }
}

/** Aggregate session options from entries already constrained by the project picker. */
export function groupHistorySessions(entries: HistoryEntry[]): HistorySessionGroup[] {
  const groups = new Map<string, HistorySessionGroup>();
  for (const entry of entries) {
    const ref = historySessionOf(entry);
    const token = historySessionToken(ref);
    const existing = groups.get(token);
    if (existing) {
      existing.count += 1;
      existing.lastMs = Math.max(existing.lastMs, entry.timestampMs);
    } else {
      groups.set(token, {
        token,
        ref,
        count: 1,
        lastMs: entry.timestampMs,
      });
    }
  }
  return [...groups.values()].sort(
    (a, b) => b.lastMs - a.lastMs || a.token.localeCompare(b.token)
  );
}

export function matchesHistorySession(entry: HistoryEntry, token: string): boolean {
  return (
    token === ALL_HISTORY_SESSIONS ||
    historySessionToken(historySessionOf(entry)) === token
  );
}

/** Short stable suffix for labels; full ids remain searchable and available in tooltips. */
export function shortSessionId(id: string): string {
  const value = id.trim();
  if (value.length <= 8) return value;
  return `${value.slice(0, 8)}…`;
}
