import { flushPromises } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  readImageDataUrl: vi.fn(),
}));

vi.mock("../../lib/ipc", () => ({ readImageDataUrl: mocks.readImageDataUrl }));
vi.mock("@tauri-apps/plugin-dialog", () => ({ open: vi.fn() }));

import { useInterjectAttachments } from "./useInterjectAttachments";

describe("useInterjectAttachments", () => {
  beforeEach(() => {
    mocks.readImageDataUrl.mockReset();
    mocks.readImageDataUrl.mockResolvedValue("data:image/png;base64,aGVsbG8=");
  });

  it("prefills referenced files, previews images, and deduplicates paths", async () => {
    const state = useInterjectAttachments();
    state.reset([
      {
        path: "/tmp/a.png",
        name: "a.png",
        size: 5,
        isImage: true,
        available: true,
      },
      {
        path: "/tmp/readme.md",
        name: "readme.md",
        size: 9,
        isImage: false,
        available: true,
      },
    ]);
    await flushPromises();

    expect(state.attachmentCount.value).toBe(2);
    expect(state.composerImages.value).toHaveLength(1);
    expect(state.composerFiles.value.map((file) => file.path)).toEqual(["/tmp/readme.md"]);

    state.appendPaths(["/tmp/readme.md", "/tmp/notes.txt"]);
    expect(state.filePaths.value).toEqual(["/tmp/a.png", "/tmp/readme.md", "/tmp/notes.txt"]);
  });

  it("keeps unavailable references visible and removable", () => {
    const state = useInterjectAttachments();
    state.reset([
      {
        path: "/tmp/gone.pdf",
        name: "gone.pdf",
        size: 12,
        isImage: false,
        available: false,
      },
    ]);

    expect(state.composerFiles.value[0].available).toBe(false);
    state.removeComposerFile(0);
    expect(state.hasAttachments.value).toBe(false);
  });
});
