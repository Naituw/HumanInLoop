// 平台探测（UA 嗅探对桌面 WebView 足够）：决定 macOS/Windows 专属 UI 与行为。
export const isMac = navigator.userAgent.toLowerCase().includes("mac");
export const isWindows = navigator.userAgent.toLowerCase().includes("win");
export const supportsAgentTasks = isMac || isWindows;
