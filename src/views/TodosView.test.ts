import { flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../i18n";
import TodosView from "./TodosView.vue";

const ipc = vi.hoisted(() => ({
  openNewTask: vi.fn(async () => {}),
  readImageDataUrl: vi.fn(async () => "data:image/png;base64,cGF0aA=="),
  todosAdd: vi.fn(async () => ({ id: "created" })),
  todosClear: vi.fn(async () => {}),
  todosComplete: vi.fn(async () => {}),
  todosHistory: vi.fn(async () => []),
  todosHistoryClear: vi.fn(async () => {}),
  todosInit: vi.fn(async () => ({
    theme: "system",
    lang: "zh",
    popupSubmitKey: "cmdEnter",
    newTaskSupported: false,
  })),
  todosList: vi.fn(async (): Promise<unknown[]> => []),
  todosProjects: vi.fn(async () => [
    { key: "/project", name: "project", count: 0, section: "recent" },
  ]),
  todosProjectsEnriched: vi.fn(async () => [
    { key: "/project", name: "project", count: 0, section: "recent" },
  ]),
  todosRemove: vi.fn(async () => {}),
  todosReorder: vi.fn(async () => {}),
  todosRestore: vi.fn(async () => {}),
  todosSetAuto: vi.fn(async () => {}),
  todosAttachPastedImages: vi.fn(async () => ({ id: "updated" })),
  todosUpdate: vi.fn(async () => ({ id: "updated" })),
  todosUpdateAttachments: vi.fn(async () => ({ id: "updated" })),
  fileIconDataUrl: vi.fn(async () => null),
  openPath: vi.fn(async () => {}),
  previewAttachments: vi.fn(async () => {}),
  showAttachmentMenu: vi.fn(async () => {}),
  todoAttachmentThumbnail: vi.fn(async () => null),
}));

const tauriEvents = vi.hoisted(() => ({
  listeners: new Map<string, (event: { payload: unknown }) => void>(),
}));

vi.mock("../lib/ipc", () => ipc);
vi.mock("../lib/theme", () => ({
  applyTheme: vi.fn(),
  fileToDataUrl: vi.fn(async () => "data:image/png;base64,cG5n"),
}));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(
    async (name: string, handler: (event: { payload: unknown }) => void) => {
      tauriEvents.listeners.set(name, handler);
      return () => tauriEvents.listeners.delete(name);
    }
  ),
}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ onDragDropEvent: vi.fn(async () => vi.fn()) }),
}));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn(async () => null) }));
vi.mock("@crabnebula/tauri-plugin-drag", () => ({
  startDrag: vi.fn(async () => {}),
}));

function pasteEvent(items: DataTransferItemList): Event {
  const event = new Event("paste", { bubbles: true, cancelable: true });
  Object.defineProperty(event, "clipboardData", { value: { items } });
  return event;
}

describe("TodosView new-todo clipboard attachments", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    tauriEvents.listeners.clear();
    i18n.global.locale.value = "zh";
  });

  afterEach(() => {
    document.body.innerHTML = "";
  });

  it("keeps text paste native and submits pasted images with the new todo", async () => {
    const wrapper = mount(TodosView, {
      attachTo: document.body,
      global: { plugins: [i18n] },
    });
    await flushPromises();
    const input = wrapper.get<HTMLTextAreaElement>(".td-input");

    const textItems = {
      0: { kind: "string", type: "text/plain", getAsFile: () => null },
      length: 1,
    } as unknown as DataTransferItemList;
    const textPaste = pasteEvent(textItems);
    input.element.dispatchEvent(textPaste);
    expect(textPaste.defaultPrevented).toBe(false);

    const png = new File(["png"], "capture.png", { type: "image/png" });
    const imageItems = {
      0: { kind: "file", type: "image/png", getAsFile: () => png },
      length: 1,
    } as unknown as DataTransferItemList;
    const imagePaste = pasteEvent(imageItems);
    input.element.dispatchEvent(imagePaste);
    await flushPromises();

    expect(imagePaste.defaultPrevented).toBe(true);
    expect(wrapper.get(".thumb").attributes("title")).toBe("capture.png");
    expect(wrapper.get(".thumb img").attributes("src")).toBe(
      "data:image/png;base64,cG5n"
    );
    await wrapper.get(".thumb .remove").trigger("click");
    expect(wrapper.find(".thumb").exists()).toBe(false);

    input.element.dispatchEvent(pasteEvent(imageItems));
    await flushPromises();

    await input.setValue("Create from clipboard");
    await wrapper.get(".td-btn-add").trigger("click");
    await flushPromises();

    expect(ipc.todosAdd).toHaveBeenCalledWith(
      "/project",
      "Create from clipboard",
      false,
      [],
      [
        {
          data: "data:image/png;base64,cG5n",
          mediaType: "image/png",
          filename: "capture.png",
        },
      ]
    );
    expect(wrapper.find(".thumb").exists()).toBe(false);
    expect(input.element.value).toBe("");
    wrapper.unmount();
  });

  it("requires a second click before removing a stored attachment", async () => {
    ipc.todosList.mockResolvedValueOnce([
      {
        id: "todo-1",
        text: "Keep this attachment safe",
        createdAtMs: Date.now(),
        attachments: [
          {
            id: "attachment-1",
            name: "capture.png",
            size: 1024,
            isImage: true,
            sourcePath: "/source/capture.png",
            path: "/managed/capture.png",
            storage: "managed",
            available: true,
          },
        ],
      },
    ]);
    const wrapper = mount(TodosView, {
      attachTo: document.body,
      global: { plugins: [i18n] },
    });
    await flushPromises();

    await wrapper.get(".todo-attachments-summary").trigger("click");
    await wrapper.get(".todo-attachment-remove").trigger("click");
    expect(ipc.todosUpdateAttachments).not.toHaveBeenCalled();
    expect(wrapper.get(".todo-attachment-remove-confirm").text()).toBe("确认删除");

    await wrapper.get(".todo-attachment-remove-confirm").trigger("click");
    await flushPromises();
    expect(ipc.todosUpdateAttachments).toHaveBeenCalledWith(
      "/project",
      "todo-1",
      [],
      ["attachment-1"]
    );
    wrapper.unmount();
  });

  it("syncs the attachment highlight from the window-level Quick Look event", async () => {
    const storedAttachment = (id: string) => ({
      id,
      name: `${id}.pdf`,
      size: 1024,
      isImage: false,
      sourcePath: `/source/${id}.pdf`,
      path: `/managed/${id}.pdf`,
      storage: "managed",
      available: true,
    });
    ipc.todosList.mockResolvedValueOnce([
      {
        id: "todo-preview",
        text: "Preview attachments",
        createdAtMs: Date.now(),
        attachments: [storedAttachment("first"), storedAttachment("second")],
      },
    ]);
    const wrapper = mount(TodosView, {
      attachTo: document.body,
      global: { plugins: [i18n] },
    });
    await flushPromises();
    await wrapper.get(".todo-attachments-summary").trigger("click");
    const rows = wrapper.findAll(".todo-attachment");
    await rows[0].trigger("click");
    await rows[0].trigger("keydown", { key: " " });

    expect(ipc.previewAttachments).toHaveBeenCalledWith(
      ["/managed/first.pdf", "/managed/second.pdf"],
      0
    );
    tauriEvents.listeners.get("preview-index")?.({ payload: 1 });
    await flushPromises();

    expect(wrapper.findAll(".todo-attachment")[0].classes()).not.toContain("selected");
    expect(wrapper.findAll(".todo-attachment")[1].classes()).toContain("selected");
    wrapper.unmount();
  });
});
