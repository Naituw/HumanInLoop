// 平台探测（UA 嗅探对桌面 WebView 足够）：决定 macOS/Windows 专属 UI 与行为。
export const isMac = navigator.userAgent.toLowerCase().includes("mac");
export const isWindows = navigator.userAgent.toLowerCase().includes("win");
export const supportsAgentTasks = isMac || isWindows;

export type DesktopPlatform = "mac" | "windows" | "linux";

export const desktopPlatform: DesktopPlatform = isMac
  ? "mac"
  : isWindows
    ? "windows"
    : "linux";

/** The user-facing primary shortcut modifier for the current desktop platform. */
export function primaryModifierLabel(platform: DesktopPlatform = desktopPlatform): string {
  return platform === "mac" ? "⌘" : "Ctrl";
}

/** Format one primary-modifier shortcut for compact button badges and settings copy. */
export function primaryShortcutLabel(
  key: string,
  platform: DesktopPlatform = desktopPlatform,
): string {
  if (platform === "mac") {
    const symbol = key.toLowerCase() === "enter" ? "↵" : key.toUpperCase();
    return `⌘${symbol}`;
  }
  const display = key.toLowerCase() === "enter" ? "Enter" : key.toUpperCase();
  return `Ctrl+${display}`;
}

/** Match only the modifier advertised by the current platform. */
export function primaryModifierPressed(
  event: Pick<KeyboardEvent, "metaKey" | "ctrlKey">,
  platform: DesktopPlatform = desktopPlatform,
): boolean {
  return platform === "mac" ? event.metaKey && !event.ctrlKey : event.ctrlKey && !event.metaKey;
}
