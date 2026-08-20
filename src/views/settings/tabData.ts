import type { Tab } from "./context";

export type SettingsLoad =
  | { type: "integration-status" }
  | { type: "pi-version" }
  | { type: "general" }
  | { type: "about" }
  | { type: "agent-tasks"; force: boolean };

/** Loads started when a settings tab becomes visible. */
export function tabLoads(tab: Tab, supportsAgentTasks: boolean): SettingsLoad[] {
  switch (tab) {
    case "general":
      return [{ type: "general" }, { type: "about" }];
    case "integration":
      return [{ type: "integration-status" }, { type: "pi-version" }];
    case "advanced":
      return supportsAgentTasks ? [{ type: "agent-tasks", force: false }] : [];
    default:
      return [];
  }
}

/**
 * Loads started after `get_settings`: local integration status always starts in
 * the background (tab-bar update badge), plus the visible tab's own work.
 */
export function mountLoads(activeTab: Tab, supportsAgentTasks: boolean): SettingsLoad[] {
  const tab = tabLoads(activeTab, supportsAgentTasks);
  if (tab.some((load) => load.type === "integration-status")) return tab;
  return [{ type: "integration-status" }, ...tab];
}
