import { enableAutoUnmount, flushPromises, mount } from "@vue/test-utils";
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vitest";
import { i18n } from "../i18n";
import { markdownReady } from "../lib/markdown";
import { applyTheme } from "../lib/theme";
import MarkdownContent from "./MarkdownContent.vue";

const renderMermaid = vi.hoisted(() => vi.fn());

vi.mock("../lib/mermaid", async (importOriginal) => {
  const original = await importOriginal<typeof import("../lib/mermaid")>();
  return { ...original, renderMermaid };
});

vi.mock("../lib/ipc", () => ({
  openPath: vi.fn(async () => {}),
}));

const renderedDocument = {
  documentUrl: "data:text/html;charset=UTF-8;base64,PGh0bWw+PC9odG1sPg==",
  width: 320,
  height: 180,
  findText: "Alpha\nBeta",
  accTitle: "Flow",
  accDescription: "",
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

enableAutoUnmount(afterEach);

async function waitForRenderCount(count: number): Promise<void> {
  await vi.waitFor(() => expect(renderMermaid).toHaveBeenCalledTimes(count));
  await flushPromises();
}

describe("MarkdownContent", () => {
  beforeAll(() => markdownReady);

  beforeEach(() => {
    renderMermaid.mockReset();
    renderMermaid.mockResolvedValue(renderedDocument);
    i18n.global.locale.value = "en";
    applyTheme("light");
  });

  it("progressively replaces a Mermaid fence and preserves its source", async () => {
    const wrapper = mount(MarkdownContent, {
      props: { source: "```mermaid\nflowchart TD\nA-->B\n```" },
      attrs: { style: "font-size: 13px" },
      global: { plugins: [i18n] },
    });
    await waitForRenderCount(1);

    expect(renderMermaid).toHaveBeenCalledWith(
      "flowchart TD\nA-->B\n",
      "light",
      13,
    );
    expect(wrapper.find(".mermaid-frame").attributes("sandbox")).toBe("");
    expect(wrapper.find(".mermaid-frame-shell").exists()).toBe(true);
    expect(wrapper.find(".mermaid-frame").attributes("title")).toBe("Flow");
    expect((wrapper.find("pre").element as HTMLElement).hidden).toBe(true);
    expect(wrapper.find("code").text()).toBe("flowchart TD\nA-->B");
    expect(wrapper.find("[data-find-atomic]").exists()).toBe(true);

    const canvas = wrapper.get(".mermaid-canvas").element as HTMLElement;
    Object.defineProperty(canvas, "clientWidth", {
      configurable: true,
      value: 300,
    });
    window.dispatchEvent(new Event("resize"));
    await vi.waitFor(() =>
      expect(wrapper.get(".mermaid-frame-shell").attributes("style")).toContain(
        "width: 300px",
      ),
    );
    expect(wrapper.get(".mermaid-frame").attributes("style")).toContain(
      "scale(0.9375)",
    );
  });

  it("switches between rendered and source find modes", async () => {
    const wrapper = mount(MarkdownContent, {
      props: { source: "```mermaid\nflowchart TD\nA-->B\n```" },
      global: { plugins: [i18n] },
    });
    await waitForRenderCount(1);

    await wrapper.find(".mermaid-source-toggle").trigger("click");
    expect((wrapper.find("pre").element as HTMLElement).hidden).toBe(false);
    expect(
      (wrapper.find(".mermaid-canvas").element as HTMLElement).hidden,
    ).toBe(true);
    expect(wrapper.find("[data-find-atomic]").exists()).toBe(false);
    expect(wrapper.find("pre").attributes("data-find-skip")).toBeUndefined();
  });

  it("keeps source visible on render failure", async () => {
    renderMermaid.mockRejectedValueOnce(new Error("bad syntax"));
    const wrapper = mount(MarkdownContent, {
      props: { source: "```mermaid\nnot a diagram\n```" },
      global: { plugins: [i18n] },
    });
    await waitForRenderCount(1);

    expect((wrapper.find("pre").element as HTMLElement).hidden).toBe(false);
    expect(wrapper.find(".mermaid-status-failed").text()).toBe(
      "Diagram could not be rendered; showing source",
    );
  });

  it("limits each Markdown body to ten diagrams", async () => {
    const source = Array.from(
      { length: 11 },
      (_, index) => `\`\`\`mermaid\nflowchart TD\nA${index}-->B${index}\n\`\`\``,
    ).join("\n");
    const wrapper = mount(MarkdownContent, {
      props: { source },
      global: { plugins: [i18n] },
    });
    expect(wrapper.findAll(".mermaid-block")).toHaveLength(11);
    await waitForRenderCount(10);

    expect(renderMermaid).toHaveBeenCalledTimes(10);
    expect(wrapper.findAll(".mermaid-status-tooMany")).toHaveLength(1);
  });

  it("redraws from source when the effective theme changes", async () => {
    mount(MarkdownContent, {
      props: { source: "```mermaid\nflowchart TD\nA-->B\n```" },
      global: { plugins: [i18n] },
    });
    await waitForRenderCount(1);
    applyTheme("dark");
    await waitForRenderCount(2);

    expect(renderMermaid).toHaveBeenLastCalledWith(
      "flowchart TD\nA-->B\n",
      "dark",
      14,
    );
    expect(renderMermaid).toHaveBeenCalledTimes(2);
  });

  it("rejects oversized source before loading the adapter", async () => {
    const source = `\`\`\`mermaid\n${"x".repeat(40_001)}\n\`\`\``;
    const wrapper = mount(MarkdownContent, {
      props: { source },
      global: { plugins: [i18n] },
    });
    await vi.waitFor(() =>
      expect(wrapper.find(".mermaid-status-tooLarge").exists()).toBe(true),
    );
    expect(renderMermaid).not.toHaveBeenCalled();
    expect((wrapper.find("pre").element as HTMLElement).hidden).toBe(false);
  });

  it("does not write a stale diagram after the source changes", async () => {
    const first = deferred<typeof renderedDocument>();
    const second = deferred<typeof renderedDocument>();
    renderMermaid
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    const wrapper = mount(MarkdownContent, {
      props: { source: "```mermaid\nflowchart TD\nOld-->Value\n```" },
      global: { plugins: [i18n] },
    });
    await waitForRenderCount(1);
    await wrapper.setProps({
      source: "```mermaid\nflowchart TD\nNew-->Value\n```",
    });
    await waitForRenderCount(2);

    first.resolve({ ...renderedDocument, accTitle: "Old" });
    await flushPromises();
    expect(wrapper.find(".mermaid-frame").exists()).toBe(false);

    second.resolve({ ...renderedDocument, accTitle: "New" });
    await vi.waitFor(() =>
      expect(wrapper.find(".mermaid-frame").attributes("title")).toBe("New"),
    );
    expect(wrapper.find("code").text()).toContain("New-->Value");
  });

  it("redraws localized controls when the locale changes", async () => {
    const wrapper = mount(MarkdownContent, {
      props: { source: "```mermaid\nflowchart TD\nA-->B\n```" },
      global: { plugins: [i18n] },
    });
    await waitForRenderCount(1);
    i18n.global.locale.value = "zh";
    await waitForRenderCount(2);

    expect(wrapper.find(".mermaid-source-toggle").text()).toBe(
      "查看 Mermaid 源码",
    );
  });
});
