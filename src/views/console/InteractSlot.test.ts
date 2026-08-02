import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import type { AgentRecord } from "../../lib/types";
import InteractSlot from "./InteractSlot.vue";

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("../../lib/ipc", () => ({ readImageDataUrl: vi.fn(async () => "") }));
vi.mock("../../lib/theme", () => ({ fileToDataUrl: vi.fn(async () => "") }));

const record: AgentRecord = {
  kind: "codex",
  sessionId: "session-1",
  startedAt: 1,
  lastActivity: 2,
  state: "working",
};

afterEach(() => {
  document.body.innerHTML = "";
});

describe("InteractSlot composer", () => {
  it("uses the popup input interaction and a vector attachment action", async () => {
    const wrapper = mount(InteractSlot, {
      props: {
        record,
        submitBareEnter: false,
        pendingText: "",
        pendingCount: 0,
        pendingAttachmentCount: 0,
        newTaskSupported: false,
      },
      attachTo: document.body,
      global: { plugins: [i18n] },
    });
    await flushPromises();

    expect(
      wrapper.get(".answer-composer .input-wrap .textarea").element.tagName.toLowerCase(),
    ).toBe("textarea");
    expect(wrapper.get(".answer-composer .img-btn svg").element.tagName.toLowerCase()).toBe("svg");
    expect(wrapper.find(".attach-button").exists()).toBe(false);
    expect(wrapper.text()).not.toContain("📎");
    wrapper.unmount();
  });
});
