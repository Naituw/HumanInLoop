import { mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import HistoryDetail from "./HistoryDetail.vue";
import { i18n } from "../i18n";
import type { HistoryEntry } from "../lib/types";

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => {}),
}));

vi.mock("@crabnebula/tauri-plugin-drag", () => ({
  startDrag: vi.fn(async () => {}),
}));

vi.mock("../lib/ipc", () => ({
  closePreview: vi.fn(async () => {}),
  fileIconDataUrl: vi.fn(async () => ""),
  openPath: vi.fn(async () => {}),
  previewAttachments: vi.fn(async () => {}),
  readImageDataUrl: vi.fn(async () => ""),
  showAttachmentMenu: vi.fn(async () => {}),
}));

describe("HistoryDetail", () => {
  beforeEach(() => {
    i18n.global.locale.value = "en";
  });

  it("shows the prompt and predefined options for a cancelled request", () => {
    const entry: HistoryEntry = {
      id: "cancelled-request",
      timestampMs: 1_700_000_000_000,
      project: "",
      source: "",
      channel: "popup",
      action: "cancel",
      isMarkdown: false,
      message: { text: "", files: [] },
      questions: [
        {
          message: "Which release channel should we use?",
          predefinedOptions: [
            { text: "Stable", recommended: true },
            { text: "Beta", recommended: false },
          ],
        },
      ],
      answers: [],
    };

    const wrapper = mount(HistoryDetail, {
      props: { entry },
      global: { plugins: [i18n] },
    });

    expect(wrapper.find(".cancelled-note").exists()).toBe(false);
    expect(wrapper.find(".q-block").isVisible()).toBe(true);
    expect(wrapper.find(".plain-body").text()).toBe(
      "Which release channel should we use?"
    );
    expect(wrapper.findAll(".option").map((option) => option.text())).toEqual([
      "RecommendedStable",
      "Beta",
    ]);
    expect(wrapper.find(".rec-badge").text()).toBe("Recommended");
    expect(wrapper.find(".unanswered").text()).toBe("Not answered");
  });

  it("routes stored Markdown prompts through the shared Mermaid component", () => {
    const entry: HistoryEntry = {
      id: "mermaid-history",
      timestampMs: 1_700_000_000_000,
      project: "",
      source: "",
      channel: "popup",
      action: "send",
      isMarkdown: true,
      message: {
        text: "```mermaid\nsequenceDiagram\nA->>B: Hello\n```",
        files: [],
      },
      questions: [],
      answers: [],
    };

    const wrapper = mount(HistoryDetail, {
      props: { entry },
      global: {
        plugins: [i18n],
        stubs: {
          MarkdownContent: {
            props: ["source"],
            template: '<div class="markdown-stub">{{ source }}</div>',
          },
        },
      },
    });

    expect(wrapper.get(".markdown-stub").text()).toContain("sequenceDiagram");
  });
});
