import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../../i18n";
import type { TodoAttachmentView } from "../../lib/types";
import TodoAttachmentList from "./TodoAttachmentList.vue";

vi.mock("@crabnebula/tauri-plugin-drag", () => ({
  startDrag: vi.fn(async () => {}),
}));

vi.mock("../../lib/ipc", () => ({
  fileIconDataUrl: vi.fn(async () => "data:image/png;base64,file"),
  openPath: vi.fn(async () => {}),
  previewAttachments: vi.fn(async () => {}),
  showAttachmentMenu: vi.fn(async () => {}),
  todoAttachmentThumbnail: vi.fn(
    async (_project: string, _todoId: string, attachmentId: string) =>
      `data:image/png;base64,${attachmentId}`
  ),
}));

function attachment(
  id: string,
  overrides: Partial<TodoAttachmentView> = {}
): TodoAttachmentView {
  return {
    id,
    name: `${id}.png`,
    size: 2048,
    isImage: true,
    sourcePath: `/source/${id}.png`,
    path: `/managed/${id}.png`,
    storage: "managed",
    available: true,
    ...overrides,
  };
}

function render(
  attachments: TodoAttachmentView[],
  removable = true,
  confirmRemoveId: string | null = null
) {
  return mount(TodoAttachmentList, {
    props: {
      project: "/project",
      todoId: "todo-1",
      attachments,
      removable,
      confirmRemoveId,
    },
    global: { plugins: [i18n] },
  });
}

describe("TodoAttachmentList", () => {
  beforeEach(() => {
    i18n.global.locale.value = "zh";
  });

  it("stays collapsed and previews at most three images", async () => {
    const wrapper = render([
      attachment("one"),
      attachment("two"),
      attachment("three"),
      attachment("four"),
    ]);
    await flushPromises();

    expect(wrapper.get(".todo-attachments-summary").text()).toContain("4 个附件");
    expect(wrapper.find(".todo-attachments-list").exists()).toBe(false);
    expect(wrapper.findAll(".todo-attachments-preview")).toHaveLength(3);
    expect(wrapper.get(".todo-attachments-more").text()).toBe("+1");
  });

  it("expands to a compact list without storage internals and removes directly", async () => {
    const wrapper = render([
      attachment("managed-image"),
      attachment("referenced-file", {
        name: "notes.txt",
        isImage: false,
        storage: "reference",
      }),
    ]);

    await wrapper.get(".todo-attachments-summary").trigger("click");
    await flushPromises();

    expect(wrapper.findAll(".todo-attachment")).toHaveLength(2);
    expect(wrapper.text()).not.toContain("托管");
    expect(wrapper.text()).not.toContain("引用");
    expect(wrapper.text()).toContain("2.0 KB");

    await wrapper.findAll(".todo-attachment-remove")[1].trigger("click");
    expect(wrapper.emitted("remove")).toEqual([["referenced-file"]]);

    await wrapper.setProps({ confirmRemoveId: "referenced-file" });
    expect(wrapper.get(".todo-attachment-remove-confirm").text()).toBe("确认删除");
    await wrapper.get(".todo-attachment-remove-confirm").trigger("click");
    expect(wrapper.emitted("remove")).toEqual([
      ["referenced-file"],
      ["referenced-file"],
    ]);
  });

  it("keeps history attachment lists read-only", async () => {
    const wrapper = render([attachment("history")], false);
    await wrapper.get(".todo-attachments-summary").trigger("click");

    expect(wrapper.find(".todo-attachment-remove").exists()).toBe(false);
  });

  it("moves the blue selection when Quick Look reports a new index", async () => {
    const wrapper = render([attachment("one"), attachment("two")]);
    await wrapper.get(".todo-attachments-summary").trigger("click");
    const rows = wrapper.findAll(".todo-attachment");
    await rows[0].trigger("click");
    await rows[0].trigger("keydown", { key: " " });

    expect(wrapper.emitted("previewStarted")).toEqual([[["one", "two"]]]);
    await wrapper.setProps({ previewSelectedId: "two" });
    expect(wrapper.findAll(".todo-attachment")[0].classes()).not.toContain("selected");
    expect(wrapper.findAll(".todo-attachment")[1].classes()).toContain("selected");
  });
});
