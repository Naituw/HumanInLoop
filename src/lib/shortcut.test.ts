import { describe, expect, it } from "vitest";
import { primaryModifierPressed, primaryShortcutLabel } from "./platform";
import {
  eventToSpec,
  formatModifierPreview,
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
    const s = eventToSpec(
      keyEvent({ key: "D", code: "KeyD", metaKey: true, shiftKey: true }),
      "mac",
    );
    expect(s).toEqual(spec({ cmd: true, shift: true, key: "d" }));
  });

  it("stores Windows Ctrl as the portable primary modifier", () => {
    const s = eventToSpec(
      keyEvent({ key: "d", code: "KeyD", ctrlKey: true }),
      "windows",
    );
    expect(s).toEqual(spec({ cmd: true, key: "d" }));
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
    expect(formatShortcut("cmd+shift+d", "mac")).toBe("⇧⌘D");
    expect(formatShortcut("ctrl+alt+enter", "mac")).toBe("⌃⌥↩");
    expect(formatShortcut("", "mac")).toBe("");
  });

  it("renders Windows and Linux shortcuts with Ctrl labels", () => {
    expect(formatShortcut("cmd+shift+d", "windows")).toBe("Ctrl+Shift+D");
    expect(formatShortcut("ctrl+alt+enter", "linux")).toBe("Ctrl+Alt+Enter");
    expect(formatModifierPreview(keyEvent({ ctrlKey: true, key: "Control" }), "windows"))
      .toBe("Ctrl+…");
  });
});

describe("primary shortcut labels", () => {
  it("uses Command symbols on macOS and Ctrl names elsewhere", () => {
    expect(primaryShortcutLabel("enter", "mac")).toBe("⌘↵");
    expect(primaryShortcutLabel("enter", "windows")).toBe("Ctrl+Enter");
    expect(primaryShortcutLabel("1", "linux")).toBe("Ctrl+1");
  });

  it("matches only the modifier shown for that platform", () => {
    const command = keyEvent({ metaKey: true });
    const control = keyEvent({ ctrlKey: true });
    expect(primaryModifierPressed(command, "mac")).toBe(true);
    expect(primaryModifierPressed(control, "mac")).toBe(false);
    expect(primaryModifierPressed(control, "windows")).toBe(true);
    expect(primaryModifierPressed(command, "windows")).toBe(false);
  });
});

describe("matchShortcut", () => {
  it("requires an exact modifier + key match", () => {
    const ev = keyEvent({ key: "d", code: "KeyD", metaKey: true });
    expect(matchShortcut(ev, "cmd+d", "mac")).toBe(true);
    expect(matchShortcut(ev, "cmd+shift+d", "mac")).toBe(false);
    expect(matchShortcut(ev, "", "mac")).toBe(false);
  });

  it("maps historical cmd and ctrl tokens to Windows Ctrl", () => {
    const control = keyEvent({ key: "d", code: "KeyD", ctrlKey: true });
    expect(matchShortcut(control, "cmd+d", "windows")).toBe(true);
    expect(matchShortcut(control, "ctrl+d", "windows")).toBe(true);
    expect(matchShortcut(keyEvent({ key: "d", code: "KeyD", metaKey: true }), "cmd+d", "windows"))
      .toBe(false);
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
    expect(shortcutConflict(spec({ cmd: true, key: "c" }))?.key).toBe("editing");
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
