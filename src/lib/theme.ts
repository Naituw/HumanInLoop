import { computed, readonly, ref } from "vue";
import type { ThemeMode, WindowEffect } from "./types";

const configuredTheme = ref<ThemeMode>("system");
const systemDark = ref(false);
let mediaQuery: MediaQueryList | null = null;
let mediaListening = false;

function syncSystemTheme(event?: MediaQueryListEvent): void {
  systemDark.value = event?.matches ?? mediaQuery?.matches ?? false;
}

function ensureSystemThemeListener(): void {
  if (mediaListening || typeof window === "undefined" || !window.matchMedia) return;
  mediaQuery = window.matchMedia("(prefers-color-scheme: dark)");
  syncSystemTheme();
  if (typeof mediaQuery.addEventListener === "function") {
    mediaQuery.addEventListener("change", syncSystemTheme);
  } else {
    // Safari 13 exposes the legacy MediaQueryList listener API.
    mediaQuery.addListener(syncSystemTheme);
  }
  mediaListening = true;
}

export const effectiveColorScheme = readonly(
  computed<"light" | "dark">(() =>
    configuredTheme.value === "dark" ||
    (configuredTheme.value === "system" && systemDark.value)
      ? "dark"
      : "light",
  ),
);

/// 套用主题：显式 light/dark 加类名，system 交给 prefers-color-scheme 兜底。
export function applyTheme(theme: ThemeMode): void {
  ensureSystemThemeListener();
  configuredTheme.value = theme;
  const root = document.documentElement;
  root.classList.remove("theme-light", "theme-dark");
  if (theme === "light") root.classList.add("theme-light");
  else if (theme === "dark") root.classList.add("theme-dark");
}

/// Apply the effective native window material to the shared document surface.
/// The `macos` layout class is intentionally independent so titlebar spacing never jumps.
export function applyWindowMaterial(effect: WindowEffect): void {
  const root = document.documentElement;
  root.classList.toggle("material-solid", effect === "solid");
  root.classList.toggle("material-translucent", effect !== "solid");
}

/// 把文件/Blob 读成 base64 data URL 字符串。
export function fileToDataUrl(file: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as string);
    reader.onerror = () => reject(reader.error);
    reader.readAsDataURL(file);
  });
}
