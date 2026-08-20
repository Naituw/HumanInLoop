import { mount, type VueWrapper } from "@vue/test-utils";
import { ref } from "vue";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import type { AgentId, AgentModeStatus } from "../../lib/types";
import IntegrationTab from "./IntegrationTab.vue";

const useSettingsContext = vi.hoisted(() => vi.fn());

vi.mock("./context", () => ({ useSettingsContext }));

function modeStatus(
  overrides: Partial<AgentModeStatus> = {},
): AgentModeStatus {
  return {
    mode: "cli",
    needsUpdate: false,
    ruleNeedsUpdate: false,
    hookNeedsUpdate: false,
    mcpNeedsUpdate: false,
    rulePath: "~/.cursor/rules/askhuman.mdc",
    ruleInstalled: true,
    timeoutHookSupported: true,
    timeoutHookInstalled: true,
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
      enabled: true,
      preferenceConfigured: true,
      installed: true,
      outdated: false,
      supported: true,
      needsUpdate: false,
      cleanupRequired: false,
    },
    stop: {
      supported: true,
      enabled: true,
      installed: true,
      outdated: false,
      otherHandlersDetected: false,
    },
    askQuestion: {
      supported: false,
      enabled: false,
      installed: false,
      outdated: false,
    },
    mcpSupported: true,
    mcpConfigPath: "~/.cursor/mcp.json",
    mcpConfigInstalled: false,
    runtimeArtifactKind: "hook",
    agentVersion: null,
    minimumVersion: null,
    versionSupported: true,
    ...overrides,
  };
}

function agentDefinition(id: AgentId, hasTimeoutHook: boolean) {
  return {
    id,
    title: id,
    hasTimeoutHook,
    hasCli: id !== "grok",
    hasMcp: id !== "pi",
    instructionKind: id === "grok" ? "skill" : "rule",
    recommended: id === "cursor" || id === "claude" || id === "pi" ? "cli" : "mcp",
  };
}

function settingsContext(
  agent: ReturnType<typeof agentDefinition>,
  status: AgentModeStatus,
) {
  return {
    config: ref(null),
    revealLabel: "Reveal",
    prompt: ref(""),
    promptCopied: ref(false),
    promptVariant: ref<"cli" | "mcp">("cli"),
    collabBusy: ref(false),
    collabError: ref(""),
    changeCollaborationStyle: vi.fn(),
    saveCustomCollaborationText: vi.fn(),
    AGENTS: [agent],
    modes: ref({ [agent.id]: status }),
    integrationLoading: ref(false),
    piVersionLoading: ref(false),
    modeBusy: ref({ [agent.id]: false }),
    modeMessage: ref({ [agent.id]: null }),
    modeError: ref({ [agent.id]: false }),
    setMode: vi.fn(),
    togglePermission: vi.fn(),
    toggleLifecycle: vi.fn(),
    toggleStop: vi.fn(),
    toggleAskQuestion: vi.fn(),
    permissionBlockedText: vi.fn(() => ""),
    updateArtifact: vi.fn(),
    updateSummary: ref({ total: 0, rule: 0, hook: 0, mcp: 0 }),
    updateAllBusy: ref(false),
    updateAll: vi.fn(),
    openMenuKey: ref(null),
    toggleOpenMenu: vi.fn(),
    closeOpenMenu: vi.fn(),
    revealFile: vi.fn(),
    openFile: vi.fn(),
    setPromptVariant: vi.fn(),
    mcpExampleJson: "",
    mcpExampleToml: "",
    mcpJsonCopied: ref(false),
    mcpTomlCopied: ref(false),
    copyMcpExample: vi.fn(),
    copyPrompt: vi.fn(),
    settingsTargetHighlight: ref(null),
  };
}

describe("IntegrationTab", () => {
  beforeEach(() => {
    i18n.global.locale.value = "en";
    useSettingsContext.mockReset();
  });

  function mountAgent(
    agent: ReturnType<typeof agentDefinition>,
    status: AgentModeStatus,
    extras: { integrationLoading?: boolean; piVersionLoading?: boolean } = {},
  ) {
    const context = settingsContext(agent, status);
    if (extras.integrationLoading) context.integrationLoading.value = true;
    if (extras.piVersionLoading) context.piVersionLoading.value = true;
    useSettingsContext.mockReturnValue(context);
    const wrapper = mount(IntegrationTab, {
      global: { plugins: [i18n] },
    });
    return { wrapper, context };
  }

  function rowByLabel(wrapper: VueWrapper, agent: AgentId, key: string) {
    const label = i18n.global.t(key);
    const row = wrapper
      .get(`#integration-${agent}`)
      .findAll(".agent-row")
      .find(
        (row) =>
          row.find(".label").exists() && row.find(".label").text() === label,
      );
    expect(row).toBeDefined();
    return row!;
  }

  it("shows default-on lifecycle drift inside an active Agent card", async () => {
    const lifecycle = {
      enabled: true,
      preferenceConfigured: false,
      installed: false,
      outdated: false,
      supported: true,
      needsUpdate: true,
      cleanupRequired: false,
    };
    const { wrapper, context } = mountAgent(
      agentDefinition("claude", true),
      modeStatus({ lifecycle }),
    );
    const row = rowByLabel(
      wrapper,
      "claude",
      "settings.integration.lifecycleTitle",
    );
    expect(row.get("input").element.checked).toBe(true);
    expect(row.find(".badge").text()).toBe(
      i18n.global.t("settings.integration.notConfigured"),
    );
    await row.get(".btn-update").trigger("click");
    expect(context.updateArtifact).toHaveBeenCalledWith("claude", "hook");
    const runtimeRow = rowByLabel(
      wrapper,
      "claude",
      "settings.integration.hookLabel",
    );
    expect(runtimeRow.find(".btn-update").exists()).toBe(false);
  });

  it("respects an explicit lifecycle opt-out without showing an update", () => {
    const lifecycle = {
      enabled: false,
      preferenceConfigured: true,
      installed: false,
      outdated: false,
      supported: true,
      needsUpdate: false,
      cleanupRequired: false,
    };
    const { wrapper } = mountAgent(
      agentDefinition("codex", false),
      modeStatus({ lifecycle }),
    );
    const row = rowByLabel(
      wrapper,
      "codex",
      "settings.integration.lifecycleTitle",
    );
    expect(row.get("input").element.checked).toBe(false);
    expect(row.find(".btn-update").exists()).toBe(false);
  });

  it("renders lifecycle tracking as the final option in an active Agent card", () => {
    const { wrapper } = mountAgent(
      agentDefinition("claude", true),
      modeStatus({
        permission: {
          supported: true,
          unsupportedReason: null,
          enabled: true,
          configured: true,
          outdated: false,
          needsUpdate: false,
          knownBlockedReason: null,
          otherHandlersDetected: false,
        },
        askQuestion: {
          supported: true,
          enabled: true,
          installed: true,
          outdated: false,
        },
      }),
    );
    const labels = wrapper
      .get("#integration-claude")
      .findAll(".agent-row .label")
      .map((label) => label.text());
    expect(labels[labels.length - 1]).toBe(
      i18n.global.t("settings.integration.lifecycleTitle"),
    );
  });

  it("prompts cleanup for legacy lifecycle in None mode", async () => {
    const lifecycle = {
      enabled: false,
      preferenceConfigured: false,
      installed: true,
      outdated: false,
      supported: true,
      needsUpdate: true,
      cleanupRequired: true,
    };
    const { wrapper, context } = mountAgent(
      agentDefinition("cursor", true),
      modeStatus({ mode: "none", lifecycle }),
    );
    const card = wrapper.get("#integration-cursor");
    expect(card.text()).toContain(
      i18n.global.t("settings.integration.lifecycleCleanupHint"),
    );
    await card.get(".lifecycle-cleanup .btn-update").trigger("click");
    expect(context.updateArtifact).toHaveBeenCalledWith("cursor", "hook");
  });

  it("hides lifecycle controls for a clean None mode", () => {
    const { wrapper } = mountAgent(
      agentDefinition("grok", false),
      modeStatus({
        mode: "none",
        lifecycle: {
          enabled: false,
          preferenceConfigured: true,
          installed: false,
          outdated: false,
          supported: true,
          needsUpdate: false,
          cleanupRequired: false,
        },
      }),
    );
    expect(wrapper.find("#lifecycle-grok").exists()).toBe(false);
  });

  it.each(["cursor", "claude"] as const)(
    "shows the installed %s CLI timeout hook without requiring a recovery hook",
    (agent) => {
      const { wrapper } = mountAgent(
        agentDefinition(agent, true),
        modeStatus({
          mode: "cli",
          timeoutHookInstalled: true,
          recoveryHookInstalled: false,
        }),
      );
      const hookRow = rowByLabel(
        wrapper,
        agent,
        "settings.integration.hookLabel",
      );

      expect(hookRow.find(".badge").text()).toBe(
        i18n.global.t("settings.integration.installed"),
      );
      expect(hookRow.find(".badge .dot").classes()).toContain("on");
    },
  );

  it("does not let a recovery hook mask a missing Cursor timeout hook", () => {
    const { wrapper } = mountAgent(
      agentDefinition("cursor", true),
      modeStatus({
        timeoutHookInstalled: false,
        recoveryHookInstalled: true,
      }),
    );
    const hookRow = rowByLabel(
      wrapper,
      "cursor",
      "settings.integration.hookLabel",
    );
    expect(hookRow.find(".badge").text()).toBe(
      i18n.global.t("settings.integration.notInstalled"),
    );
    expect(hookRow.find(".badge .dot").classes()).toContain("off");
  });

  it.each([false, true])(
    "shows Codex CLI recovery hook readiness when installed=%s",
    (installed) => {
      const { wrapper } = mountAgent(
        agentDefinition("codex", false),
        modeStatus({
          mode: "cli",
          timeoutHookSupported: false,
          timeoutHookInstalled: false,
          recoveryHookInstalled: installed,
        }),
      );
      const hookRow = rowByLabel(
        wrapper,
        "codex",
        "settings.integration.contextRecoveryHookLabel",
      );
      expect(hookRow.find(".badge").text()).toBe(
        i18n.global.t(
          installed
            ? "settings.integration.installed"
            : "settings.integration.notInstalled",
        ),
      );
      expect(hookRow.find(".badge .dot").classes()).toContain(
        installed ? "on" : "off",
      );
    },
  );

  it("uses the aggregate CLI hook update action without changing timeout readiness", async () => {
    const { wrapper, context } = mountAgent(
      agentDefinition("claude", true),
      modeStatus({
        timeoutHookInstalled: true,
        recoveryHookInstalled: false,
        hookNeedsUpdate: true,
      }),
    );
    const hookRow = rowByLabel(
      wrapper,
      "claude",
      "settings.integration.hookLabel",
    );
    expect(hookRow.find(".badge").text()).toBe(
      i18n.global.t("settings.integration.installed"),
    );
    await hookRow.get(".btn-update").trigger("click");
    expect(context.updateArtifact).toHaveBeenCalledWith("claude", "hook");
  });

  it.each(["cursor", "claude", "codex", "grok"] as const)(
    "routes %s MCP recovery drift through the MCP artifact",
    async (agent) => {
      const { wrapper, context } = mountAgent(
        agentDefinition(agent, agent === "cursor" || agent === "claude"),
        modeStatus({
          mode: "mcp",
          mcpConfigInstalled: true,
          mcpNeedsUpdate: true,
          recoveryHookInstalled: false,
        }),
      );
      const mcpRow = rowByLabel(
        wrapper,
        agent,
        "settings.integration.mcpConfigLabel",
      );
      expect(mcpRow.find(".badge").text()).toBe(
        i18n.global.t("settings.integration.installed"),
      );
      await mcpRow.get(".btn-update").trigger("click");
      expect(context.updateArtifact).toHaveBeenCalledWith(agent, "mcp");
    },
  );

  it("shows Pi as CLI-only and labels the managed runtime as an Extension", () => {
    const { wrapper } = mountAgent(
      agentDefinition("pi", true),
      modeStatus({
        mode: "cli",
        mcpSupported: false,
        runtimeArtifactKind: "extension",
        agentVersion: "0.82.0",
        minimumVersion: "0.82.0",
      }),
    );
    const card = wrapper.get("#integration-pi");
    const modeLabels = card.findAll(".seg").map((button) => button.text());
    expect(modeLabels).toContain(i18n.global.t("settings.integration.modeCli") + i18n.global.t("settings.integration.recommendedTag"));
    expect(modeLabels).not.toContain(i18n.global.t("settings.integration.modeMcp"));
    rowByLabel(wrapper, "pi", "settings.integration.extensionLabel");
    expect(card.text()).toContain(
      i18n.global.t("settings.integration.piPermissionUnsupported"),
    );
  });

  it("reports an unsupported Pi version in the integration card", () => {
    const { wrapper } = mountAgent(
      agentDefinition("pi", true),
      modeStatus({
        mode: "cli",
        mcpSupported: false,
        runtimeArtifactKind: "extension",
        agentVersion: "0.81.9",
        minimumVersion: "0.82.0",
        versionSupported: false,
      }),
    );
    expect(wrapper.get("#integration-pi").text()).toContain("0.81.9");
    expect(wrapper.get("#integration-pi").text()).toContain("0.82.0");
  });

  it("hides agent cards while integration status is loading", () => {
    const { wrapper } = mountAgent(
      agentDefinition("pi", true),
      modeStatus({ mode: "none" }),
      { integrationLoading: true },
    );
    expect(wrapper.find("#integration-pi").exists()).toBe(false);
    expect(wrapper.text()).toContain(i18n.global.t("common.loading"));
  });

  it("does not show a Pi version error while the version probe is in flight", () => {
    const { wrapper } = mountAgent(
      agentDefinition("pi", true),
      modeStatus({
        mode: "cli",
        mcpSupported: false,
        runtimeArtifactKind: "extension",
        versionSupported: false,
        minimumVersion: "0.82.0",
      }),
      { piVersionLoading: true },
    );
    const card = wrapper.get("#integration-pi");
    expect(card.text()).toContain(i18n.global.t("common.loading"));
    expect(card.text()).not.toContain(
      i18n.global.t("settings.integration.piVersionMissing", {
        minimum: "0.82.0",
      }),
    );
  });
});
