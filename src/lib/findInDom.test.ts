import { describe, expect, it } from "vitest";
import {
  applyFindMarks,
  clearFindMarks,
  findAllRanges,
  findHighlightableRanges,
  setAtomicFindText,
  setCurrentFindMark,
} from "./findInDom";

describe("findAllRanges", () => {
  it("finds case-insensitive non-overlapping ranges", () => {
    expect(findAllRanges("Foo foo FOO", "foo", false)).toEqual([
      { start: 0, end: 3 },
      { start: 4, end: 7 },
      { start: 8, end: 11 },
    ]);
  });

  it("respects case sensitivity", () => {
    expect(findAllRanges("Foo foo", "Foo", true)).toEqual([
      { start: 0, end: 3 },
    ]);
  });

  it("returns empty for empty query", () => {
    expect(findAllRanges("abc", "", false)).toEqual([]);
  });

  it("handles overlapping-style consecutive matches by advancing past needle", () => {
    expect(findAllRanges("aaaa", "aa", false)).toEqual([
      { start: 0, end: 2 },
      { start: 2, end: 4 },
    ]);
  });
});

describe("applyFindMarks / clearFindMarks", () => {
  it("wraps matches and clears them", () => {
    const root = document.createElement("div");
    root.textContent = "hello world hello";
    const marks = applyFindMarks(root, "hello", false);
    expect(marks).toHaveLength(2);
    expect(root.querySelectorAll("mark").length).toBe(2);
    expect(root.textContent).toBe("hello world hello");

    setCurrentFindMark(marks, 1);
    expect(marks[1]!.classList.contains("popup-find-hit-current")).toBe(true);
    expect(marks[0]!.classList.contains("popup-find-hit-current")).toBe(false);

    clearFindMarks(root);
    expect(root.querySelectorAll("mark").length).toBe(0);
    expect(root.textContent).toBe("hello world hello");
  });

  it("skips textarea content", () => {
    const root = document.createElement("div");
    root.innerHTML = `<p>visible needle</p><textarea>needle</textarea>`;
    const marks = applyFindMarks(root, "needle", false);
    expect(marks).toHaveLength(1);
    expect(root.querySelector("textarea")!.value).toBe("needle");
  });

  it("treats a rendered diagram as one atomic match without touching its SVG", () => {
    const root = document.createElement("div");
    root.innerHTML =
      '<p>needle</p><div data-find-atomic><iframe></iframe><svg><text>keep</text></svg></div>';
    const diagram = root.querySelector<HTMLElement>("[data-find-atomic]")!;
    setAtomicFindText(diagram, "needle needle");
    const before = diagram.querySelector("svg")!.outerHTML;

    expect(findHighlightableRanges(root, "needle", false)).toHaveLength(2);
    const marks = applyFindMarks(root, "needle", false);
    expect(marks).toHaveLength(2);
    expect(diagram.querySelectorAll("[data-popup-find]")).toHaveLength(1);
    expect(diagram.querySelector("svg")!.outerHTML).toBe(before);

    setCurrentFindMark(marks, 1);
    expect(diagram.classList.contains("popup-find-atomic-current")).toBe(true);
    clearFindMarks(root);
    expect(diagram.querySelector("svg")!.outerHTML).toBe(before);
    expect(diagram.classList.contains("popup-find-atomic-hit")).toBe(false);
  });
});
