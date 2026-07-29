import { afterEach, describe, expect, it, vi } from "vitest";
import {
  inputMayShrinkTextarea,
  resizeTextareaToContent,
  settleTextareaHeightAfterBlur,
} from "./textareaAutosize";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("resizeTextareaToContent", () => {
  it("grows through one final height write without a temporary auto height", async () => {
    const textarea = document.createElement("textarea");
    textarea.style.height = "64px";
    document.body.appendChild(textarea);
    vi.spyOn(textarea, "getBoundingClientRect").mockReturnValue({
      width: 320,
      height: 64,
    } as DOMRect);
    Object.defineProperty(textarea, "scrollHeight", {
      configurable: true,
      value: 96,
    });
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver((records) => mutations.push(...records));
    observer.observe(textarea, { attributes: true, attributeFilter: ["style"] });

    resizeTextareaToContent(textarea, true, false);
    await Promise.resolve();

    expect(textarea.style.height).toBe("96px");
    expect(mutations).toHaveLength(1);
    expect(mutations[0]?.oldValue).not.toBe("auto");
    observer.disconnect();
    textarea.remove();
  });

  it("does not touch height when an insertion stays on the same line", async () => {
    const textarea = document.createElement("textarea");
    textarea.style.height = "96px";
    document.body.appendChild(textarea);
    vi.spyOn(textarea, "getBoundingClientRect").mockReturnValue({
      width: 320,
      height: 96,
    } as DOMRect);
    Object.defineProperty(textarea, "scrollHeight", {
      configurable: true,
      value: 96,
    });
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver((records) => mutations.push(...records));
    observer.observe(textarea, { attributes: true, attributeFilter: ["style"] });

    resizeTextareaToContent(textarea, true, false);
    await Promise.resolve();

    expect(textarea.style.height).toBe("96px");
    expect(mutations).toHaveLength(0);
    observer.disconnect();
    textarea.remove();
  });

  it("measures possible shrinkage without mutating the live textarea through auto", async () => {
    const textarea = document.createElement("textarea");
    textarea.style.height = "120px";
    document.body.appendChild(textarea);
    vi.spyOn(textarea, "getBoundingClientRect").mockReturnValue({
      width: 320,
      height: 120,
    } as DOMRect);
    vi.spyOn(
      HTMLTextAreaElement.prototype,
      "scrollHeight",
      "get"
    ).mockImplementation(function (this: HTMLTextAreaElement) {
      return this === textarea ? 120 : 72;
    });
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver((records) => mutations.push(...records));
    observer.observe(textarea, { attributes: true, attributeFilter: ["style"] });

    resizeTextareaToContent(textarea, true, true);
    await Promise.resolve();

    expect(textarea.style.height).toBe("72px");
    expect(mutations).toHaveLength(1);
    expect(document.body.querySelectorAll('textarea[aria-hidden="true"]')).toHaveLength(0);
    observer.disconnect();
    textarea.remove();
  });
});

describe("inputMayShrinkTextarea", () => {
  it("keeps ordinary text and line-break insertions on the grow-only path", () => {
    expect(
      inputMayShrinkTextarea(new InputEvent("input", { inputType: "insertText" }))
    ).toBe(false);
    expect(
      inputMayShrinkTextarea(
        new InputEvent("input", { inputType: "insertLineBreak" })
      )
    ).toBe(false);
  });

  it("remeasures deletions, undo, and replacement-like insertions", () => {
    for (const inputType of [
      "deleteContentBackward",
      "historyUndo",
      "insertReplacementText",
      "insertFromPaste",
    ]) {
      expect(
        inputMayShrinkTextarea(new InputEvent("input", { inputType }))
      ).toBe(true);
    }
  });
});

describe("settleTextareaHeightAfterBlur", () => {
  it("does not mutate the measured height of an expanded editor", async () => {
    const textarea = document.createElement("textarea");
    textarea.style.height = "62px";
    const mutations: MutationRecord[] = [];
    const observer = new MutationObserver((records) => mutations.push(...records));
    observer.observe(textarea, { attributes: true, attributeFilter: ["style"] });

    settleTextareaHeightAfterBlur(textarea, true);
    await Promise.resolve();

    expect(textarea.style.height).toBe("62px");
    expect(mutations).toHaveLength(0);
    observer.disconnect();
  });

  it("clears the measured height when the editor collapses", () => {
    const textarea = document.createElement("textarea");
    textarea.style.height = "62px";

    settleTextareaHeightAfterBlur(textarea, false);

    expect(textarea.style.height).toBe("");
  });
});
