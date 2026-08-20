// Agent task settings: enable confirmation, readiness, and working-directory management.
import { nextTick, onBeforeUnmount, ref } from "vue";
import { useI18n } from "vue-i18n";
import { open as openDialog } from "@tauri-apps/plugin-dialog";
import {
  agentTaskReadiness,
  agentTaskTestTerminal,
  agentTaskWorkspaceAdd,
  agentTaskWorkspaceForget,
  agentTaskWorkspaceHide,
  agentTaskWorkspacePin,
  agentTaskWorkspaces,
  openPath,
} from "../../lib/ipc";
import type { AgentKind, AgentTaskReadiness, AgentTaskWorkspace } from "../../lib/types";
import { supportsAgentTasks } from "../../lib/platform";
import type { SettingsCore } from "./context";

/** Agent 官方安装文档（readiness「CLI ×」跳转；新建任务窗口复用）。 */
export const AGENT_INSTALL_DOCS: Record<AgentKind, string> = {
  claude: "https://docs.anthropic.com/en/docs/claude-code/getting-started",
  codex: "https://developers.openai.com/codex/cli/",
  cursor: "https://cursor.com/docs/cli/installation",
  grok: "https://docs.x.ai/build/overview",
  pi: "https://github.com/earendil-works/pi",
};

export function useAgentTasks(core: SettingsCore) {
  const { t } = useI18n();
  const { config, activeTab, persist } = core;

  const taskWorkspaces = ref<AgentTaskWorkspace[]>([]);
  const taskReadiness = ref<AgentTaskReadiness[]>([]);
  const taskSettingsBusy = ref(false);
  const taskSettingsMessage = ref("");
  const workspacePanelOpen = ref(false);
  const workspaceMenuPath = ref<string | null>(null);
  let taskSettingsInflight: Promise<void> | null = null;
  let piReadinessInflight: Promise<AgentTaskReadiness | undefined> | null = null;

  async function refreshAgentTaskSettings(scan = false) {
    taskSettingsBusy.value = true;
    taskSettingsMessage.value = "";
    try {
      [taskWorkspaces.value, taskReadiness.value] = await Promise.all([
        agentTaskWorkspaces(scan),
        agentTaskReadiness({ force: scan }),
      ]);
    } catch (e) {
      taskSettingsMessage.value = String(e);
    } finally {
      taskSettingsBusy.value = false;
    }
  }

  async function ensureAgentTaskSettings(force = false) {
    if (!supportsAgentTasks) return;
    if (taskSettingsInflight) {
      await taskSettingsInflight;
      if (!force) return;
    }
    const run = refreshAgentTaskSettings(force);
    taskSettingsInflight = run.finally(() => {
      if (taskSettingsInflight === run) taskSettingsInflight = null;
    });
    await taskSettingsInflight;
  }

  async function ensurePiReadiness(): Promise<AgentTaskReadiness | undefined> {
    const existing = taskReadiness.value.find((item) => item.kind === "pi");
    if (existing) return existing;
    if (piReadinessInflight) return piReadinessInflight;
    const run = (async () => {
      const items = await agentTaskReadiness({ kind: "pi" });
      const item = items.find((entry) => entry.kind === "pi") ?? items[0];
      if (item) {
        const rest = taskReadiness.value.filter((entry) => entry.kind !== "pi");
        taskReadiness.value = [...rest, item];
      }
      return item;
    })();
    piReadinessInflight = run.finally(() => {
      if (piReadinessInflight === run) piReadinessInflight = null;
    });
    return piReadinessInflight;
  }

  // 开启走确认弹层（列出保活/登录项等副作用，用户点「继续开启」才生效）；关闭直接持久化。
  const agentTasksConfirmOpen = ref(false);

  async function toggleAgentTasks() {
    if (!config.value) return;
    if (config.value.agentTasks.enabled) {
      // 先回退开关，待弹层确认后再真正开启。
      config.value.agentTasks.enabled = false;
      agentTasksConfirmOpen.value = true;
      return;
    }
    await persist();
    await refreshAgentTaskSettings(false);
  }

  async function confirmEnableAgentTasks() {
    agentTasksConfirmOpen.value = false;
    if (!config.value) return;
    config.value.agentTasks.enabled = true;
    config.value.general.daemonLifecycle = "keepalive";
    await persist();
    await refreshAgentTaskSettings(false);
  }

  async function pickTaskWorkspace() {
    if (taskSettingsBusy.value) return;
    taskSettingsBusy.value = true;
    taskSettingsMessage.value = "";
    try {
      const selection = await openDialog({
        directory: true,
        multiple: false,
        title: t("settings.agentTasks.chooseWorkspace"),
      });
      const path = Array.isArray(selection) ? selection[0] : selection;
      if (!path) return;
      await agentTaskWorkspaceAdd(path);
      await refreshAgentTaskSettings(false);
    } catch (e) {
      taskSettingsMessage.value = String(e);
    } finally {
      taskSettingsBusy.value = false;
    }
  }

  async function mutateTaskWorkspace(
    action: "pin" | "hide" | "forget",
    workspace: AgentTaskWorkspace,
  ) {
    workspaceMenuPath.value = null;
    taskSettingsMessage.value = "";
    try {
      if (action === "pin") await agentTaskWorkspacePin(workspace.path, !workspace.pinned);
      if (action === "hide") await agentTaskWorkspaceHide(workspace.path, !workspace.hidden);
      if (action === "forget") await agentTaskWorkspaceForget(workspace.path);
      await refreshAgentTaskSettings(false);
    } catch (e) {
      taskSettingsMessage.value = String(e);
    }
  }

  function openWorkspacePanel() {
    workspaceMenuPath.value = null;
    taskSettingsMessage.value = "";
    workspacePanelOpen.value = true;
    // 冷扫描只在真正管理工作目录时做（onMounted 不扫）：扫描要读五家 Agent 的会话元数据，
    // 打开设置页就扫既浪费也曾连环触发 macOS 文件权限弹窗。
    void refreshAgentTaskSettings(true);
  }

  function closeWorkspacePanel() {
    workspaceMenuPath.value = null;
    workspacePanelOpen.value = false;
  }

  function toggleWorkspaceMenu(path: string) {
    workspaceMenuPath.value = workspaceMenuPath.value === path ? null : path;
  }

  function workspaceAgents(workspace: AgentTaskWorkspace): string {
    if (workspace.agents.length === 0) return t("settings.agentTasks.manuallyAdded");
    const labels: Record<AgentKind, string> = {
      claude: "Claude Code",
      codex: "Codex",
      cursor: "Cursor",
      grok: "Grok",
      pi: "Pi",
    };
    return workspace.agents.map((kind) => labels[kind]).join(" · ");
  }

  function workspaceLastUsed(workspace: AgentTaskWorkspace): string {
    if (!workspace.lastUsedAt) return "";
    return t("settings.agentTasks.lastUsed", {
      time: new Date(workspace.lastUsedAt * 1000).toLocaleString(),
    });
  }

  type ReadinessIssue = "binary" | "lifecycle" | "integration";
  const settingsTargetHighlight = ref("");
  let settingsTargetTimer: number | undefined;

  /** 滚动定位 + 短暂高亮当前 tab 内的目标元素（须已切到目标 tab）。跨窗口锚点跳转
   *（`open_settings` 的 `tab#elementId`，spec gui-agent-task-launch G5）也复用本函数。 */
  async function gotoSettingsTarget(target: string) {
    await nextTick();
    settingsTargetHighlight.value = target;
    document.getElementById(target)?.scrollIntoView({ behavior: "smooth", block: "center" });
    if (settingsTargetTimer) window.clearTimeout(settingsTargetTimer);
    settingsTargetTimer = window.setTimeout(() => {
      settingsTargetHighlight.value = "";
      settingsTargetTimer = undefined;
    }, 2200);
  }

  async function openReadinessIssue(kind: AgentKind, issue: ReadinessIssue) {
    if (issue === "binary") {
      await openPath(AGENT_INSTALL_DOCS[kind]);
      return;
    }
    activeTab.value = "integration";
    await gotoSettingsTarget(
      issue === "lifecycle" ? `lifecycle-${kind}` : `integration-${kind}`,
    );
  }

  onBeforeUnmount(() => {
    if (settingsTargetTimer) window.clearTimeout(settingsTargetTimer);
  });

  async function testAgentTaskTerminal() {
    taskSettingsMessage.value = "";
    try {
      await agentTaskTestTerminal();
      taskSettingsMessage.value = t("settings.agentTasks.terminalTestDone");
    } catch (e) {
      taskSettingsMessage.value = String(e);
    }
  }

  return {
    taskWorkspaces,
    taskReadiness,
    taskSettingsBusy,
    taskSettingsMessage,
    workspacePanelOpen,
    workspaceMenuPath,
    refreshAgentTaskSettings,
    ensureAgentTaskSettings,
    ensurePiReadiness,
    agentTasksConfirmOpen,
    toggleAgentTasks,
    confirmEnableAgentTasks,
    pickTaskWorkspace,
    mutateTaskWorkspace,
    openWorkspacePanel,
    closeWorkspacePanel,
    toggleWorkspaceMenu,
    workspaceAgents,
    workspaceLastUsed,
    settingsTargetHighlight,
    gotoSettingsTarget,
    openReadinessIssue,
    testAgentTaskTerminal,
  };
}
