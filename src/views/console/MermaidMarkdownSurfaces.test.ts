import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import type { AgentDetailFrame } from "../../lib/types";
import TranscriptPane from "./TranscriptPane.vue";
import WatchPane from "./WatchPane.vue";

const { consoleTranscriptMock } = vi.hoisted(() => ({
  consoleTranscriptMock: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({
  consoleTranscript: consoleTranscriptMock,
}));

const MarkdownStub = {
  props: {
    source: { type: String, required: true },
    lazyMermaid: Boolean,
  },
  template:
    '<div class="markdown-stub" :data-source="source" :data-lazy="String(lazyMermaid)" />',
};

describe("Agent console Mermaid Markdown surfaces", () => {
  beforeEach(() => {
    i18n.global.locale.value = "en";
    consoleTranscriptMock.mockReset();
  });

  it("routes Watch text through the shared Mermaid-capable component", () => {
    const frame: AgentDetailFrame = {
      type: "watchFrame",
      sessionId: "session-1",
      seq: 1,
      kindLabel: "Codex",
      phase: "working",
      text: "```mermaid\nstateDiagram-v2\n[*] --> Ready\n```",
      steps: [],
      stepsOmitted: 0,
      todos: [],
    };

    const wrapper = mount(WatchPane, {
      props: { frame },
      global: {
        plugins: [i18n],
        stubs: { MarkdownContent: MarkdownStub },
      },
    });

    expect(wrapper.get(".markdown-stub").attributes("data-source")).toContain(
      "stateDiagram-v2",
    );
    expect(wrapper.get(".markdown-stub").attributes("data-lazy")).toBe(
      "false",
    );
  });

  it("lazy-routes assistant and AskHuman message Markdown in transcripts", async () => {
    consoleTranscriptMock.mockResolvedValue({
      events: [
        {
          type: "assistant",
          text: "```mermaid\nclassDiagram\nClass01 <|-- AveryLongClass\n```",
        },
        {
          type: "ask",
          kind: "ask",
          message: "```mermaid\nmindmap\n  root((plan))\n```",
          questions: [],
        },
      ],
      start: 0,
      total: 2,
      truncatedHead: false,
      partial: false,
    });

    const wrapper = mount(TranscriptPane, {
      props: { kind: "codex", sessionId: "session-1" },
      global: {
        plugins: [i18n],
        stubs: { MarkdownContent: MarkdownStub },
      },
    });
    await flushPromises();

    const markdown = wrapper.findAll(".markdown-stub");
    expect(markdown).toHaveLength(2);
    expect(markdown[0].attributes("data-source")).toContain("classDiagram");
    expect(markdown[1].attributes("data-source")).toContain("mindmap");
    expect(markdown.every((node) => node.attributes("data-lazy") === "true"))
      .toBe(true);
  });
});
