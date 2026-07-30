import { describe, expect, it } from "vitest";
import {
  applyFindMarks,
  clearFindMarks,
  findAllRanges,
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
});
