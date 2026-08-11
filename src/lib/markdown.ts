import type MarkdownIt from "markdown-it";
import { shallowRef } from "vue";

// R4 bundle 瘦身：markdown-it（连带 entities/linkify-it 等依赖约 170KB）不进主 chunk，
// 模块加载时立刻发起动态 import（与主 chunk 剩余执行并行）。渲染器就绪前 renderMarkdown
// 退回「转义 + 保留换行」的纯文本；就绪时 mdRef 变化会让所有依赖它的 computed 自动重算，
// 调用方无需感知（预热路径下窗口显示前早已就绪，冷路径闪替也在毫秒级）。
const mdRef = shallowRef<MarkdownIt | null>(null);

/** Resolves when the real renderer is ready (tests await this; app code relies on reactivity). */
export const markdownReady: Promise<void> = import("markdown-it").then(
  ({ default: MarkdownItCtor }) => {
    const md = new MarkdownItCtor({
      html: false,
      linkify: true,
      breaks: true,
    });

    const defaultFence = md.renderer.rules.fence?.bind(md.renderer);
    md.renderer.rules.fence = (tokens, idx, options, env, self) => {
      const rendered = defaultFence
        ? defaultFence(tokens, idx, options, env, self)
        : self.renderToken(tokens, idx, options);
      const language = tokens[idx]?.info.trim().split(/\s+/, 1)[0]?.toLowerCase();
      return wrapCodeBlock(md, rendered, env, language === "mermaid");
    };

    const defaultCodeBlock = md.renderer.rules.code_block?.bind(md.renderer);
    md.renderer.rules.code_block = (tokens, idx, options, env, self) => {
      const rendered = defaultCodeBlock
        ? defaultCodeBlock(tokens, idx, options, env, self)
        : self.renderToken(tokens, idx, options);
      return wrapCodeBlock(md, rendered, env);
    };

    mdRef.value = md;
  },
);

/** Labels for the per-code-block copy button (localized by the caller). */
export interface MarkdownOptions {
  copyLabel?: string;
  copiedLabel?: string;
}

// Copy / done icons reused for the code-block copy button (stroke-based, theme-aware).
const COPY_ICON =
  '<svg class="cc-ico cc-copy" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>';
const CHECK_ICON =
  '<svg class="cc-ico cc-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';

// Wrap a rendered <pre>…</pre> code block so it can host a hover copy button.
// The raw text is read from the DOM at click time (see handleCodeCopyClick), so
// nothing extra needs to be embedded here beyond the localized labels.
function wrapCodeBlock(
  md: MarkdownIt,
  rendered: string,
  env: unknown,
  isMermaid = false,
): string {
  const e = (env ?? {}) as MarkdownOptions;
  const copy = md.utils.escapeHtml(e.copyLabel ?? "Copy");
  const copied = md.utils.escapeHtml(e.copiedLabel ?? "Copied");
  const button =
    `<button class="code-copy" type="button" title="${copy}" aria-label="${copy}"` +
    ` data-copy="${copy}" data-copied="${copied}">${COPY_ICON}${CHECK_ICON}</button>`;
  const marker = isMermaid ? " mermaid-block" : "";
  const pending = isMermaid ? " data-mermaid-pending" : "";
  return `<div class="code-block${marker}"${pending}>${rendered}${button}</div>`;
}

// Minimal HTML escape for the pre-load fallback (mirrors markdown-it's escapeHtml set).
function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function renderMarkdown(source: string, opts?: MarkdownOptions): string {
  const md = mdRef.value;
  if (!md) {
    // Renderer chunk still loading: escaped plain text with line breaks preserved
    // (matches breaks:true), upgraded reactively the moment the chunk lands.
    return `<p>${escapeHtml(source).replace(/\n/g, "<br>\n")}</p>`;
  }
  return md.render(source, { ...opts });
}

// Delegated click handler for code-block copy buttons. Returns true when the
// click hit a copy button (so callers can stop further handling). Reads the
// code text from the sibling <code> and shows a brief "copied" state.
export function handleCodeCopyClick(e: MouseEvent): boolean {
  const target = e.target as HTMLElement | null;
  const btn = target?.closest?.(".code-copy") as HTMLElement | null;
  if (!btn) return false;
  const handledEvent = e as MouseEvent & { __askHumanCodeCopyHandled?: boolean };
  if (handledEvent.__askHumanCodeCopyHandled) return true;
  handledEvent.__askHumanCodeCopyHandled = true;
  e.preventDefault();
  e.stopPropagation();
  const code = btn.closest(".code-block")?.querySelector("code");
  const text = code?.textContent ?? "";

  const showCopied = () => {
    btn.classList.add("copied");
    const copied = btn.getAttribute("data-copied");
    if (copied) btn.setAttribute("title", copied);
    window.setTimeout(() => {
      btn.classList.remove("copied");
      const copy = btn.getAttribute("data-copy");
      if (copy) btn.setAttribute("title", copy);
    }, 1500);
  };

  navigator.clipboard.writeText(text).then(showCopied).catch(() => {
    /* Clipboard unavailable: ignore silently. */
  });
  return true;
}
