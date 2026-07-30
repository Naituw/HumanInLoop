import { describe, expect, it } from "vitest";
import {
  eventToSpec,
  formatShortcut,
  isModifierOnly,
  matchShortcut,
  parseShortcut,
  shortcutConflict,
  specToString,
  type ShortcutSpec,
} from "./shortcut";

function keyEvent(init: KeyboardEventInit): KeyboardEvent {
  return new KeyboardEvent("keydown", init);
}

function spec(partial: Partial<ShortcutSpec>): ShortcutSpec {
  return { cmd: false, ctrl: false, alt: false, shift: false, key: "d", ...partial };
}

describe("eventToSpec", () => {
  it("normalizes the physical key so Shift does not change the result", () => {
    const s = eventToSpec(keyEvent({ key: "D", code: "KeyD", metaKey: true, shiftKey: true }));
    expect(s).toEqual(spec({ cmd: true, shift: true, key: "d" }));
  });

  it("maps digits and punctuation codes", () => {
    expect(eventToSpec(keyEvent({ code: "Digit1", key: "1", metaKey: true }))?.key).toBe("1");
    expect(eventToSpec(keyEvent({ code: "BracketLeft", key: "[", metaKey: true }))?.key).toBe("[");
    expect(eventToSpec(keyEvent({ code: "NumpadEnter", key: "Enter" }))?.key).toBe("enter");
  });

  it("returns null for modifier-only presses and unmapped keys", () => {
    expect(eventToSpec(keyEvent({ key: "Shift", code: "ShiftLeft" }))).toBeNull();
    expect(eventToSpec(keyEvent({ key: "F5", code: "F5" }))).toBeNull();
  });
});

describe("isModifierOnly", () => {
  it("detects bare modifiers", () => {
    expect(isModifierOnly(keyEvent({ key: "Meta" }))).toBe(true);
    expect(isModifierOnly(keyEvent({ key: "d" }))).toBe(false);
  });
});

describe("specToString / parseShortcut", () => {
  it("round-trips with modifiers in canonical order", () => {
    const s = spec({ cmd: true, shift: true, key: "d" });
    expect(specToString(s)).toBe("cmd+shift+d");
    expect(parseShortcut("cmd+shift+d")).toEqual(s);
  });

  it("returns null for the empty string", () => {
    expect(parseShortcut("")).toBeNull();
  });
});

describe("formatShortcut", () => {
  it("renders macOS-style symbols", () => {
    expect(formatShortcut("cmd+shift+d")).toBe("⇧⌘D");
    expect(formatShortcut("ctrl+alt+enter")).toBe("⌃⌥↩");
    expect(formatShortcut("")).toBe("");
  });
});

describe("matchShortcut", () => {
  it("requires an exact modifier + key match", () => {
    const ev = keyEvent({ key: "d", code: "KeyD", metaKey: true });
    expect(matchShortcut(ev, "cmd+d")).toBe(true);
    expect(matchShortcut(ev, "cmd+shift+d")).toBe(false);
    expect(matchShortcut(ev, "")).toBe(false);
  });
});

describe("shortcutConflict", () => {
  it("requires cmd or ctrl", () => {
    expect(shortcutConflict(spec({ alt: true }))?.key).toBe("needMod");
  });

  it("rejects keys reserved by the popup", () => {
    expect(shortcutConflict(spec({ cmd: true, key: "enter" }))?.key).toBe("enter");
    expect(shortcutConflict(spec({ cmd: true, key: "w" }))?.key).toBe("cancel");
    expect(shortcutConflict(spec({ cmd: true, key: "[" }))?.key).toBe("brackets");
    expect(shortcutConflict(spec({ cmd: true, key: "3" }))?.key).toBe("options");
  });

  it("rejects bare editing shortcuts but allows them with extra modifiers", () => {
    expect(shortcutConflict(spec({ cmd: true, key: "c" }))).toEqual({
      key: "editing",
      params: { key: "C" },
    });
    expect(shortcutConflict(spec({ cmd: true, shift: true, key: "v" }))).toBeNull();
  });

  it("rejects ⌘F reserved for in-page find", () => {
    expect(shortcutConflict(spec({ cmd: true, key: "f" }))?.key).toBe("find");
    expect(shortcutConflict(spec({ ctrl: true, key: "f" }))?.key).toBe("find");
    expect(shortcutConflict(spec({ cmd: true, shift: true, key: "f" }))).toBeNull();
  });

  it("accepts a normal combo", () => {
    expect(shortcutConflict(spec({ cmd: true, key: "d" }))).toBeNull();
  });
});
