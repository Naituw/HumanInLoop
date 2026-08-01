import { mount } from "@vue/test-utils";
import { describe, expect, it } from "vitest";
import { i18n } from "../i18n";
import ComposerAttachments from "./ComposerAttachments.vue";

describe("ComposerAttachments", () => {
  it("shares thumbnail and file-chip removal behavior across composers", async () => {
    const wrapper = mount(ComposerAttachments, {
      props: {
        images: [
          {
            key: "image-1",
            data: "data:image/png;base64,cG5n",
            filename: "capture.png",
          },
        ],
        files: [{ key: "file-1", path: "/tmp/notes.txt", name: "notes.txt" }],
      },
      global: { plugins: [i18n] },
    });

    expect(wrapper.get(".thumb img").attributes("src")).toContain("data:image/png");
    expect(wrapper.get(".reply-file").text()).toContain("notes.txt");

    await wrapper.get(".thumb .remove").trigger("click");
    await wrapper.get(".reply-file .rf-remove").trigger("click");
    expect(wrapper.emitted("removeImage")).toEqual([[0]]);
    expect(wrapper.emitted("removeFile")).toEqual([[0]]);
  });
});
