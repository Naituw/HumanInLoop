import { flushPromises, mount } from "@vue/test-utils";
import { ref } from "vue";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import PermissionRulesCard from "./PermissionRulesCard.vue";

const permissionRulesPanel = vi.hoisted(() => vi.fn());
const useSettingsContext = vi.hoisted(() => vi.fn());

vi.mock("../../lib/ipc", () => ({ permissionRulesPanel }));
vi.mock("./context", () => ({ useSettingsContext }));

const session = {
  summary: {
    sessionId: "019f99b6-20d2-7aa0-af9b-55033676aeb0",
    ruleCount: 3,
    fileExactCount: 0,
    projectRoots: [],
    fullDisk: true,
    shellCount: 0,
    networkCount: 0,
    mcpCount: 1,
    yolo: true,
    lastUsedAtMs: 1_784_993_873_486,
  },
  title: "Investigate permission prompts",
  projectName: "HumanInLoop",
};

describe("PermissionRulesCard", () => {
  beforeEach(() => {
    document.body.innerHTML = '<div class="settings"></div><div id="mount"></div>';
    i18n.global.locale.value = "en";
    permissionRulesPanel.mockReset();
    permissionRulesPanel.mockImplementation(async (op: { op: string }) => {
      if (op.op === "summaries") {
        return { kind: "summaries", sessions: [structuredClone(session)], globalCount: 0 };
      }
      if (op.op === "sessionDetail") {
        return {
          kind: "rules",
          rules: [
            {
              kind: "fileDisk",
              display: "",
              createdAtMs: 1,
              lastUsedAtMs: 1,
              expiresAtMs: 2,
            },
          ],
        };
      }
      return { kind: "reset", removed: 3 };
    });
    useSettingsContext.mockReturnValue({
      config: ref({ permissions: { codexRelaxedShell: false } }),
      persist: vi.fn(async () => {}),
    });
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("keeps the card compact and manages titled sessions in a sheet", async () => {
    const wrapper = mount(PermissionRulesCard, {
      attachTo: "#mount",
      global: { plugins: [i18n] },
    });

    expect(wrapper.find(".permission-list").exists()).toBe(false);
    await wrapper.get(".permission-rules-card .btn").trigger("click");
    await flushPromises();

    const panel = document.querySelector(".permission-panel") as HTMLElement;
    expect(panel).not.toBeNull();
    expect(panel.textContent).toContain("Investigate permission prompts");
    expect(panel.textContent).toContain("HumanInLoop");
    expect(panel.textContent).toContain("YOLO");
    expect(panel.textContent).toContain("Full disk");

    const row = panel.querySelector(".permission-list-row") as HTMLElement;
    expect(row.querySelectorAll(".workspace-more-button")).toHaveLength(1);
    expect(row.querySelectorAll(":scope > .btn")).toHaveLength(0);

    (row.querySelector(".permission-row-disclosure") as HTMLButtonElement).click();
    await flushPromises();
    expect(permissionRulesPanel).toHaveBeenCalledWith({
      op: "sessionDetail",
      sessionId: session.summary.sessionId,
    });
    expect(panel.textContent).toContain("Full disk");

    wrapper.unmount();
  });

  it("moves destructive actions into the ellipsis menu and confirms reset", async () => {
    const wrapper = mount(PermissionRulesCard, {
      attachTo: "#mount",
      global: { plugins: [i18n] },
    });
    await wrapper.get(".permission-rules-card .btn").trigger("click");
    await flushPromises();

    const panel = document.querySelector(".permission-panel") as HTMLElement;
    (panel.querySelector(".workspace-more-button") as HTMLButtonElement).click();
    await flushPromises();
    const menu = panel.querySelector(".permission-menu-pop") as HTMLElement;
    expect(menu.textContent).toContain("Turn off YOLO");
    expect(menu.textContent).toContain("Reset");

    const reset = Array.from(menu.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Reset",
    ) as HTMLButtonElement;
    reset.click();
    await flushPromises();
    const dialog = document.querySelector(".permission-reset-dialog") as HTMLElement;
    expect(dialog.textContent).toContain("Investigate permission prompts");
    expect(dialog.textContent).toContain("Permanent rules");

    const confirm = Array.from(dialog.querySelectorAll("button")).find(
      (button) => button.textContent?.trim() === "Reset Grants",
    ) as HTMLButtonElement;
    confirm.click();
    await flushPromises();
    expect(permissionRulesPanel).toHaveBeenCalledWith({
      op: "resetSession",
      sessionId: session.summary.sessionId,
    });
    expect(panel.querySelector(".permission-list-row")).toBeNull();

    wrapper.unmount();
  });
});
