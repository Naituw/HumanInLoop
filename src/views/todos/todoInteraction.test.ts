import { describe, expect, it } from "vitest";
import { clipboardImageFiles, nextTodoSelection } from "./todoInteraction";

describe("nextTodoSelection", () => {
  const ids = ["first", "second", "third"];

  it("enters from the nearest edge and clamps at both ends", () => {
    expect(nextTodoSelection(ids, null, 1)).toBe("first");
    expect(nextTodoSelection(ids, null, -1)).toBe("third");
    expect(nextTodoSelection(ids, "first", -1)).toBe("first");
    expect(nextTodoSelection(ids, "third", 1)).toBe("third");
  });

  it("moves between rows and recovers from stale selection", () => {
    expect(nextTodoSelection(ids, "first", 1)).toBe("second");
    expect(nextTodoSelection(ids, "third", -1)).toBe("second");
    expect(nextTodoSelection(ids, "removed", 1)).toBe("first");
    expect(nextTodoSelection([], null, 1)).toBeNull();
  });
});

describe("clipboardImageFiles", () => {
  it("keeps only clipboard file items with an image MIME type", () => {
    const png = new File(["png"], "capture.png", { type: "image/png" });
    const items = {
      0: { kind: "string", type: "text/plain", getAsFile: () => null },
      1: { kind: "file", type: "application/pdf", getAsFile: () => null },
      2: { kind: "file", type: "image/png", getAsFile: () => png },
      length: 3,
    } as unknown as DataTransferItemList;

    expect(clipboardImageFiles(items)).toEqual([png]);
    expect(clipboardImageFiles()).toEqual([]);
  });
});
