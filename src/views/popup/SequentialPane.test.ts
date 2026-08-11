import { mount } from "@vue/test-utils";
import { ref } from "vue";
import { describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import SequentialPane from "./SequentialPane.vue";
import { PopupCtxKey, type PopupContext } from "./context";

function mountPane(whatsNext: boolean, markdown = false) {
  const options = [
    {
      text: "Run todo: ship the fix 【2 attachments】",
      recommended: false,
      todoId: "todo-1",
      todoText: "ship the fix",
      todoAttachments: [
        {
          id: "attachment-1",
          name: "one.txt",
          path: "/tmp/one.txt",
          sourcePath: "/tmp/one.txt",
          storage: "reference",
        },
        {
          id: "attachment-2",
          name: "two.txt",
          path: "/tmp/two.txt",
          sourcePath: "/tmp/two.txt",
          storage: "reference",
        },
      ],
    },
    { text: "Review logs", recommended: true },
  ];
  const ctx = {
    request: ref({ whatsNext, isMarkdown: markdown }),
    showQuestionHeader: ref(false),
    showDescription: ref(false),
    questionHeaderLabel: ref("Question"),
    qHeaderRef: ref<HTMLElement | null>(null),
    transitionName: ref("none"),
    onQuestionEntered: vi.fn(),
    current: ref(0),
    currentQuestion: ref({
      message: markdown ? "```mermaid\nflowchart TD\nA-->B\n```" : "",
      predefinedOptions: options,
    }),
    renderedHtml: ref(""),
    viewSource: ref(false),
    onContentClick: vi.fn(),
    chosen: ref<string[]>([]),
    single: ref(false),
    selectOnly: ref(true),
    optionHotkey: vi.fn(() => null),
    toggle: vi.fn(),
  } as unknown as PopupContext;

  return mount(SequentialPane, {
    global: {
      plugins: [i18n],
      provide: { [PopupCtxKey as symbol]: ctx },
      stubs: {
        AnswerComposer: true,
        MarkdownContent: {
          props: ["source"],
          template: '<div class="markdown-stub">{{ source }}</div>',
        },
        Transition: false,
      },
    },
  });
}

describe("SequentialPane todo badge", () => {
  it("marks only todo options in a whats-next request", () => {
    const wrapper = mountPane(true);
    const rows = wrapper.findAll(".option");
    expect(rows[0].get(".todo-option-badge").text()).toBe("TODO");
    expect(rows[0].text()).toContain("ship the fix");
    expect(rows[0].get(".todo-attachment-badge").text()).toBe(
      "【2 attachments】",
    );
    expect(rows[0].text()).not.toContain("Run todo:");
    expect(rows[1].find(".todo-option-badge").exists()).toBe(false);
  });

  it("does not mark todo-id options outside whats-next", () => {
    const wrapper = mountPane(false);
    expect(wrapper.find(".todo-option-badge").exists()).toBe(false);
    expect(wrapper.text()).toContain("Run todo: ship the fix");
  });

  it("routes Markdown questions through the shared Mermaid-capable component", () => {
    const wrapper = mountPane(false, true);
    expect(wrapper.get(".markdown-stub").text()).toContain("flowchart TD");
    expect(wrapper.find(".plain-body").exists()).toBe(false);
  });
});
