// 「Agent 集成」域：各 Agent 的 CLI/MCP/未集成三态、单项与批量更新、
// 停止确认与权限确认开关、参考提示词与 MCP 手动配置示例。
import { computed, ref } from "vue";
import { useI18n } from "vue-i18n";
import {
  agentModeStatus,
  agentModeSet,
  agentModeUpdate,
  agentModeUpdateArtifact,
  agentPermissionSet,
  agentAskQuestionSet,
  agentStopSet,
  agentLifecycleInstall,
  agentLifecycleUninstall,
  agentRuleReveal,
  agentRuleOpen,
  mcpConfigReveal,
  mcpConfigOpen,
  mcpCommandPath,
  agentHookReveal,
  agentHookOpen,
  getPrompt,
  collaborationStyleDefaults,
  collaborationStyleApplyIntegrations,
} from "../../lib/ipc";
import type {
  AgentId,
  AgentMode,
  AgentModeStatus,
  AgentTaskReadiness,
  CollaborationStyle,
} from "../../lib/types";
import { isMac, isWindows, supportsAgentTasks } from "../../lib/platform";
import type { SettingsCore } from "./context";

const PI_MINIMUM_VERSION = "0.82.0";

export function useIntegration(
  core: SettingsCore,
  tasks: {
    refreshAgentTaskSettings: (scan?: boolean) => Promise<void>;
    ensurePiReadiness: () => Promise<AgentTaskReadiness | undefined>;
    taskReadiness: { value: AgentTaskReadiness[] };
  },
) {
  const { t } = useI18n();
  const { config, persist } = core;
  const { refreshAgentTaskSettings, ensurePiReadiness, taskReadiness } = tasks;

  // 「在文件管理器中显示」的按平台措辞（访达 / 文件资源管理器 / 文件管理器），单一来源。
  const revealLabel = computed(() => {
    if (isMac) return t("settings.integration.revealInFinder");
    if (isWindows) return t("settings.integration.revealInExplorer");
    return t("settings.integration.revealInFileManager");
  });

  const prompt = ref("");
  const promptCopied = ref(false);
  // 手动集成卡的提示词变体：CLI 版 / MCP 版（切换即重载正文）。
  const promptVariant = ref<"cli" | "mcp">("cli");

  // 各 Agent 的展示信息。Codex 没有「超时 Hook」概念（hasTimeoutHook=false），
  // 且无法延长 CLI 超时，故推荐 MCP；Cursor / Claude Code 有可靠超时 Hook，推荐 CLI。
  // `hasCli`：是否提供 CLI 档（超时 Hook 方案）。Grok 的 Composer CLI 会自动后台化，
  // 超时不可靠，故仅 None|Mcp 两态。`instructionKind`：指令载体（规则文件 vs skill）。
  const AGENTS: {
    id: AgentId;
    title: string;
    hasTimeoutHook: boolean;
    hasCli: boolean;
    hasMcp: boolean;
    instructionKind: "rule" | "skill";
    recommended: AgentMode;
  }[] = [
    { id: "cursor", title: "Cursor", hasTimeoutHook: true, hasCli: true, hasMcp: true, instructionKind: "rule", recommended: "cli" },
    { id: "claude", title: "Claude Code", hasTimeoutHook: true, hasCli: true, hasMcp: true, instructionKind: "rule", recommended: "cli" },
    { id: "codex", title: "Codex", hasTimeoutHook: false, hasCli: true, hasMcp: true, instructionKind: "rule", recommended: "mcp" },
    { id: "grok", title: "Grok", hasTimeoutHook: false, hasCli: false, hasMcp: true, instructionKind: "skill", recommended: "mcp" },
    { id: "pi", title: "Pi", hasTimeoutHook: true, hasCli: true, hasMcp: false, instructionKind: "rule", recommended: "cli" },
  ];

  const emptyMode = (): AgentModeStatus => ({
    mode: "none",
    needsUpdate: false,
    ruleNeedsUpdate: false,
    hookNeedsUpdate: false,
    mcpNeedsUpdate: false,
    rulePath: "",
    ruleInstalled: false,
    timeoutHookSupported: false,
    timeoutHookInstalled: false,
    timeoutHookNeedsUpdate: false,
    recoveryHookInstalled: false,
    permission: {
      supported: false,
      unsupportedReason: "native_permission_request_unsupported",
      enabled: false,
      configured: false,
      outdated: false,
      needsUpdate: false,
      knownBlockedReason: null,
      otherHandlersDetected: false,
    },
    permissionNeedsUpdate: false,
    lifecycle: {
      enabled: false,
      preferenceConfigured: false,
      installed: false,
      outdated: false,
      supported: true,
      needsUpdate: false,
      cleanupRequired: false,
    },
    stop: {
      supported: false,
      enabled: false,
      installed: false,
      outdated: false,
      otherHandlersDetected: false,
    },
    askQuestion: {
      supported: false,
      enabled: false,
      installed: false,
      outdated: false,
    },
    mcpSupported: false,
    mcpConfigPath: "",
    mcpConfigInstalled: false,
    runtimeArtifactKind: "hook",
    agentVersion: null,
    minimumVersion: null,
    versionSupported: true,
  });
  const modes = ref<Record<AgentId, AgentModeStatus>>({
    cursor: emptyMode(),
    claude: emptyMode(),
    codex: emptyMode(),
    grok: emptyMode(),
    pi: emptyMode(),
  });
  const modeBusy = ref<Record<AgentId, boolean>>({
    cursor: false,
    claude: false,
    codex: false,
    grok: false,
    pi: false,
  });
  const modeMessage = ref<Record<AgentId, string | null>>({
    cursor: null,
    claude: null,
    codex: null,
    grok: null,
    pi: null,
  });
  const modeError = ref<Record<AgentId, boolean>>({
    cursor: false,
    claude: false,
    codex: false,
    grok: false,
    pi: false,
  });

  function overlayPiVersion(item: AgentTaskReadiness) {
    modes.value.pi = {
      ...modes.value.pi,
      agentVersion: item.version,
      minimumVersion: PI_MINIMUM_VERSION,
      versionSupported: item.binaryReady,
    };
  }

  function overlayPiVersionIfKnown() {
    const item = taskReadiness.value.find((entry) => entry.kind === "pi");
    if (item) overlayPiVersion(item);
  }

  async function refreshMode(agent: AgentId) {
    modes.value[agent] = await agentModeStatus(agent);
    if (agent === "pi") overlayPiVersionIfKnown();
  }

  const integrationLoading = ref(true);
  const piVersionLoading = ref(false);
  let integrationInflight: Promise<void> | null = null;
  let integrationReady = false;
  let piVersionInflight: Promise<void> | null = null;
  let piVersionReady = false;

  async function ensureIntegration() {
    if (integrationReady) return;
    if (integrationInflight) return integrationInflight;
    const run = initIntegration().finally(() => {
      if (integrationInflight === run) integrationInflight = null;
      integrationReady = true;
      integrationLoading.value = false;
    });
    integrationInflight = run;
    return run;
  }

  async function ensurePiVersion() {
    if (piVersionReady) return;
    if (piVersionInflight) return piVersionInflight;
    const run = (async () => {
      piVersionLoading.value = true;
      try {
        const item = await ensurePiReadiness();
        if (item) overlayPiVersion(item);
      } finally {
        piVersionLoading.value = false;
        piVersionReady = true;
      }
    })();
    piVersionInflight = run.finally(() => {
      if (piVersionInflight === run) piVersionInflight = null;
    });
    return piVersionInflight;
  }

  // 一键切换到目标模式（含「未集成」）：自动卸旧装新。
  async function setMode(agent: AgentId, mode: AgentMode) {
    if (modeBusy.value[agent]) return;
    modeBusy.value[agent] = true;
    modeMessage.value[agent] = null;
    try {
      await agentModeSet(agent, mode);
      modeError.value[agent] = false;
    } catch (e) {
      modeMessage.value[agent] = String(e);
      modeError.value[agent] = true;
    } finally {
      modeBusy.value[agent] = false;
      await refreshMode(agent);
      if (supportsAgentTasks) await refreshAgentTaskSettings(false);
    }
  }

  async function toggleLifecycle(agent: AgentId, enabled: boolean) {
    if (modeBusy.value[agent]) return;
    modeBusy.value[agent] = true;
    modeMessage.value[agent] = null;
    try {
      if (enabled) await agentLifecycleInstall(agent);
      else await agentLifecycleUninstall(agent);
      modeError.value[agent] = false;
    } catch (e) {
      modeMessage.value[agent] = String(e);
      modeError.value[agent] = true;
    } finally {
      modeBusy.value[agent] = false;
      await refreshMode(agent);
      if (supportsAgentTasks) await refreshAgentTaskSettings(false);
    }
  }

  async function togglePermission(agent: AgentId, enabled: boolean) {
    if (modeBusy.value[agent]) return;
    modeBusy.value[agent] = true;
    modeMessage.value[agent] = null;
    try {
      await agentPermissionSet(agent, enabled);
      modeError.value[agent] = false;
    } catch (e) {
      modeMessage.value[agent] = String(e);
      modeError.value[agent] = true;
    } finally {
      modeBusy.value[agent] = false;
      await refreshMode(agent);
    }
  }

  // 接管 Claude 内置提问工具（spec claude-ask-user-question D3）。
  async function toggleAskQuestion(agent: AgentId, enabled: boolean) {
    if (modeBusy.value[agent]) return;
    modeBusy.value[agent] = true;
    modeMessage.value[agent] = null;
    try {
      await agentAskQuestionSet(agent, enabled);
      modeError.value[agent] = false;
    } catch (e) {
      modeMessage.value[agent] = String(e);
      modeError.value[agent] = true;
    } finally {
      modeBusy.value[agent] = false;
      await refreshMode(agent);
    }
  }

  async function toggleStop(agent: AgentId, enabled: boolean) {
    if (modeBusy.value[agent]) return;
    modeBusy.value[agent] = true;
    modeMessage.value[agent] = null;
    try {
      await agentStopSet(agent, enabled);
      modeError.value[agent] = false;
    } catch (e) {
      modeMessage.value[agent] = String(e);
      modeError.value[agent] = true;
    } finally {
      modeBusy.value[agent] = false;
      await refreshMode(agent);
    }
  }

  function permissionBlockedText(reason: string): string {
    return reason === "allow_managed_hooks_only"
      ? t("settings.integration.permissionManagedOnly")
      : t("settings.integration.permissionHooksDisabled");
  }

  // 单项更新：只把某个产物（rule / hook / mcp）刷新到最新。
  async function updateArtifact(agent: AgentId, artifact: "rule" | "hook" | "mcp") {
    if (modeBusy.value[agent]) return;
    modeBusy.value[agent] = true;
    modeMessage.value[agent] = null;
    try {
      await agentModeUpdateArtifact(agent, artifact);
      modeError.value[agent] = false;
    } catch (e) {
      modeMessage.value[agent] = String(e);
      modeError.value[agent] = true;
    } finally {
      modeBusy.value[agent] = false;
      await refreshMode(agent);
      if (artifact === "hook" && supportsAgentTasks) {
        await refreshAgentTaskSettings(false);
      }
    }
  }

  // 跨所有 Agent 的「待更新」概览统计：分别数出有多少家 Rule / Hook / MCP 配置过期或缺失。
  const updateSummary = computed(() => {
    let rule = 0;
    let hook = 0;
    let mcp = 0;
    for (const a of AGENTS) {
      const m = modes.value[a.id];
      if (m.ruleNeedsUpdate) rule++;
      if (m.hookNeedsUpdate) hook++;
      if (m.mcpNeedsUpdate) mcp++;
    }
    return { rule, hook, mcp, total: rule + hook + mcp };
  });

  const updateAllBusy = ref(false);

  // 一键更新所有 Agent 的过期产物（逐家调用整模式更新，幂等）。
  async function updateAll() {
    if (updateAllBusy.value) return;
    updateAllBusy.value = true;
    try {
      for (const a of AGENTS) {
        if (!modes.value[a.id].needsUpdate) continue;
        try {
          await agentModeUpdate(a.id);
          modeError.value[a.id] = false;
        } catch (e) {
          modeMessage.value[a.id] = String(e);
          modeError.value[a.id] = true;
        }
        await refreshMode(a.id);
      }
    } finally {
      updateAllBusy.value = false;
      if (supportsAgentTasks) await refreshAgentTaskSettings(false);
    }
  }

  // 「打开」下拉菜单：当前展开菜单的 key（`${agent}:${kind}`，null = 全部收起）。
  type FileKind = "rule" | "hook" | "mcp";
  const openMenuKey = ref<string | null>(null);
  function toggleOpenMenu(key: string) {
    openMenuKey.value = openMenuKey.value === key ? null : key;
  }
  function closeOpenMenu() {
    openMenuKey.value = null;
  }
  function revealFile(agent: AgentId, kind: FileKind) {
    if (kind === "mcp") mcpConfigReveal(agent);
    else if (kind === "hook") agentHookReveal(agent);
    else agentRuleReveal(agent);
    closeOpenMenu();
  }
  function openFile(agent: AgentId, kind: FileKind) {
    if (kind === "mcp") mcpConfigOpen(agent);
    else if (kind === "hook") agentHookOpen(agent);
    else agentRuleOpen(agent);
    closeOpenMenu();
  }

  async function loadPrompt() {
    prompt.value = await getPrompt(promptVariant.value);
  }

  function setPromptVariant(v: "cli" | "mcp") {
    if (promptVariant.value === v) return;
    promptVariant.value = v;
    void loadPrompt();
  }

  // —— 协作风格（全局；切换后重写已开集成 + 刷新手动 Prompt）——
  const collabBusy = ref(false);
  const collabError = ref<string | null>(null);
  const collabDefaults = ref({ aligned: "", autonomous: "" });

  async function ensureCollabDefaults() {
    if (collabDefaults.value.aligned) return;
    try {
      collabDefaults.value = await collaborationStyleDefaults();
    } catch {
      /* ignore */
    }
  }

  async function changeCollaborationStyle(style: CollaborationStyle) {
    if (!config.value || collabBusy.value) return;
    await ensureCollabDefaults();
    if (
      style === "custom" &&
      !config.value.general.collaborationStyleCustomText?.trim()
    ) {
      config.value.general.collaborationStyleCustomText =
        collabDefaults.value.aligned;
    }
    config.value.general.collaborationStyle = style;
    collabBusy.value = true;
    collabError.value = null;
    try {
      await persist();
      await collaborationStyleApplyIntegrations();
      await loadPrompt();
      await Promise.all(AGENTS.map((a) => refreshMode(a.id)));
    } catch (e) {
      collabError.value = String(e);
    } finally {
      collabBusy.value = false;
    }
  }

  async function saveCustomCollaborationText() {
    if (!config.value || collabBusy.value) return;
    collabBusy.value = true;
    collabError.value = null;
    try {
      await persist();
      if (config.value.general.collaborationStyle === "custom") {
        await collaborationStyleApplyIntegrations();
        await loadPrompt();
        await Promise.all(AGENTS.map((a) => refreshMode(a.id)));
      }
    } catch (e) {
      collabError.value = String(e);
    } finally {
      collabBusy.value = false;
    }
  }

  // MCP 手动配置示例。直接填入当前可执行文件绝对路径（与自动集成写入一致），免用户手改；
  // 取不到时退回占位符。
  const MCP_EXE_PLACEHOLDER = "<absolute path to AskHuman>";
  const mcpExePath = ref(MCP_EXE_PLACEHOLDER);
  const mcpExampleJson = computed(
    () => `{
  "mcpServers": {
    "askhuman": {
      "command": "${mcpExePath.value}",
      "args": ["mcp"],
      "timeout": 86400000
    }
  }
}`,
  );
  const mcpExampleToml = computed(
    () => `[mcp_servers.askhuman]
command = "${mcpExePath.value}"
args = ["mcp"]
startup_timeout_sec = 30
tool_timeout_sec = 86400`,
  );
  const mcpJsonCopied = ref(false);
  const mcpTomlCopied = ref(false);

  async function copyMcpExample(kind: "json" | "toml") {
    const text = kind === "json" ? mcpExampleJson.value : mcpExampleToml.value;
    try {
      await navigator.clipboard.writeText(text);
    } catch {
      /* 忽略：剪贴板不可用时静默 */
    }
    const flag = kind === "json" ? mcpJsonCopied : mcpTomlCopied;
    flag.value = true;
    setTimeout(() => (flag.value = false), 1500);
  }

  async function copyPrompt() {
    try {
      await navigator.clipboard.writeText(prompt.value);
    } catch {
      /* 忽略：剪贴板不可用时静默 */
    }
    promptCopied.value = true;
    setTimeout(() => (promptCopied.value = false), 1500);
  }

  // 集成域初始化：加载提示词、可执行路径与各 Agent 集成状态。
  async function initIntegration() {
    await ensureCollabDefaults();
    await loadPrompt();
    try {
      mcpExePath.value = await mcpCommandPath();
    } catch {
      /* 取不到路径时保留占位符 */
    }
    await Promise.all(AGENTS.map((a) => refreshMode(a.id)));
  }

  return {
    revealLabel,
    prompt,
    promptCopied,
    promptVariant,
    collabBusy,
    collabError,
    collabDefaults,
    changeCollaborationStyle,
    saveCustomCollaborationText,
    AGENTS,
    modes,
    integrationLoading,
    piVersionLoading,
    ensureIntegration,
    ensurePiVersion,
    modeBusy,
    modeMessage,
    modeError,
    refreshMode,
    setMode,
    togglePermission,
    toggleLifecycle,
    toggleStop,
    toggleAskQuestion,
    permissionBlockedText,
    updateArtifact,
    updateSummary,
    updateAllBusy,
    updateAll,
    openMenuKey,
    toggleOpenMenu,
    closeOpenMenu,
    revealFile,
    openFile,
    setPromptVariant,
    mcpExampleJson,
    mcpExampleToml,
    mcpJsonCopied,
    mcpTomlCopied,
    copyMcpExample,
    copyPrompt,
    initIntegration,
  };
}
