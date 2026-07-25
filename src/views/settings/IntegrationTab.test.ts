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
    mcpConfigPath: "~/.cursor/mcp.json",
    mcpConfigInstalled: false,
    ...overrides,
  };
}

function agentDefinition(id: AgentId, hasTimeoutHook: boolean) {
  return {
    id,
    title: id,
    hasTimeoutHook,
    hasCli: id !== "grok",
    instructionKind: id === "grok" ? "skill" : "rule",
    recommended: id === "cursor" || id === "claude" ? "cli" : "mcp",
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
    modeBusy: ref({ [agent.id]: false }),
    modeMessage: ref({ [agent.id]: null }),
    modeError: ref({ [agent.id]: false }),
    setMode: vi.fn(),
    togglePermission: vi.fn(),
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
  ) {
    const context = settingsContext(agent, status);
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
});
