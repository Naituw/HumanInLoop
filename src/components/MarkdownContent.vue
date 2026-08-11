<script setup lang="ts">
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from "vue";
import { useI18n } from "vue-i18n";
import type { MermaidDocument } from "../lib/mermaid";
import {
  MERMAID_MAX_DIAGRAMS,
  MERMAID_MAX_TEXT_SIZE,
  mermaidFitScale,
} from "../lib/mermaidLimits";
import { clearAtomicFindText, setAtomicFindText } from "../lib/findInDom";
import { openPath } from "../lib/ipc";
import { handleCodeCopyClick, renderMarkdown } from "../lib/markdown";
import { loadMermaidAdapter } from "../lib/mermaidLoader";
import { effectiveColorScheme } from "../lib/theme";

const props = withDefaults(
  defineProps<{
    source: string;
    enableMermaid?: boolean;
    lazyMermaid?: boolean;
  }>(),
  {
    enableMermaid: true,
    lazyMermaid: false,
  },
);

const { t, locale } = useI18n();
const root = ref<HTMLElement | null>(null);
const mounted = ref(false);
let generation = 0;
let observer: IntersectionObserver | null = null;
let diagramCleanups: Array<() => void> = [];

const labels = computed(() => ({
  copyLabel: t("common.copyCode"),
  copiedLabel: t("common.copied"),
}));
const html = computed(() => renderMarkdown(props.source, labels.value));

function rootHeight(): number {
  return root.value?.getBoundingClientRect().height ?? 0;
}

function announceUpdate(heightDelta = 0): void {
  root.value?.dispatchEvent(
    new CustomEvent("markdown-content-updated", {
      bubbles: true,
      detail: { heightDelta },
    }),
  );
}

function statusText(code: "rendering" | "failed" | "tooLarge" | "tooMany"): string {
  return t(`common.mermaid.${code}`);
}

function diagramFontSize(wrapper: HTMLElement): number {
  const value = Number.parseFloat(window.getComputedStyle(wrapper).fontSize);
  return Number.isFinite(value) && value > 0 ? value : 14;
}

function clearDiagramEffects(): void {
  for (const cleanup of diagramCleanups.splice(0)) cleanup();
}

function makeResponsiveDiagram(
  canvas: HTMLElement,
  shell: HTMLElement,
  iframe: HTMLIFrameElement,
  document: MermaidDocument,
  fontSize: number,
): () => void {
  let animationFrame = 0;
  const update = (): void => {
    const availableWidth = canvas.clientWidth;
    if (availableWidth <= 0) return;
    const scale = mermaidFitScale(document.width, availableWidth, fontSize);
    shell.style.width = `${Math.ceil(document.width * scale)}px`;
    shell.style.height = `${Math.ceil(document.height * scale)}px`;
    iframe.style.transform = scale === 1 ? "" : `scale(${scale})`;
  };
  const scheduleUpdate = (): void => {
    cancelAnimationFrame(animationFrame);
    animationFrame = requestAnimationFrame(update);
  };

  shell.style.width = `${Math.ceil(document.width)}px`;
  shell.style.height = `${Math.ceil(document.height)}px`;
  let resizeObserver: ResizeObserver | null = null;
  if (typeof ResizeObserver !== "undefined") {
    resizeObserver = new ResizeObserver(scheduleUpdate);
    resizeObserver.observe(canvas);
  }
  window.addEventListener("resize", scheduleUpdate);
  scheduleUpdate();

  return () => {
    cancelAnimationFrame(animationFrame);
    resizeObserver?.disconnect();
    window.removeEventListener("resize", scheduleUpdate);
  };
}

function makeStatus(
  wrapper: HTMLElement,
  code: "rendering" | "failed" | "tooLarge" | "tooMany",
): HTMLElement {
  const status = document.createElement("div");
  status.className = `mermaid-status mermaid-status-${code}`;
  status.setAttribute("data-find-skip", "");
  status.setAttribute("role", code === "rendering" ? "status" : "alert");
  status.textContent = statusText(code);
  wrapper.appendChild(status);
  return status;
}

function makeToggle(wrapper: HTMLElement): HTMLButtonElement {
  const button = document.createElement("button");
  button.className = "mermaid-source-toggle";
  button.type = "button";
  button.setAttribute("data-find-skip", "");
  wrapper.appendChild(button);
  return button;
}

function diagramLabel(document: MermaidDocument): string {
  return (
    document.accTitle ||
    document.accDescription ||
    t("common.mermaid.diagram")
  );
}

function installDiagram(
  wrapper: HTMLElement,
  pre: HTMLElement,
  document: MermaidDocument,
  token: number,
  fontSize: number,
): void {
  if (token !== generation || !root.value?.contains(wrapper)) return;
  const beforeHeight = rootHeight();

  wrapper.querySelector(".mermaid-status")?.remove();
  const canvas = window.document.createElement("div");
  canvas.className = "mermaid-canvas";
  const shell = window.document.createElement("div");
  shell.className = "mermaid-frame-shell";
  const iframe = window.document.createElement("iframe");
  iframe.className = "mermaid-frame";
  iframe.setAttribute("sandbox", "");
  iframe.setAttribute("referrerpolicy", "no-referrer");
  iframe.setAttribute("scrolling", "no");
  iframe.setAttribute("title", diagramLabel(document));
  iframe.style.width = `${Math.ceil(document.width)}px`;
  iframe.style.height = `${Math.ceil(document.height)}px`;
  iframe.src = document.documentUrl;
  shell.appendChild(iframe);
  canvas.appendChild(shell);
  wrapper.insertBefore(canvas, pre);
  diagramCleanups.push(
    makeResponsiveDiagram(canvas, shell, iframe, document, fontSize),
  );

  const toggle = makeToggle(wrapper);
  let showingSource = false;
  const syncMode = (notify = true): void => {
    const modeBeforeHeight = rootHeight();
    pre.hidden = !showingSource;
    canvas.hidden = showingSource;
    toggle.textContent = showingSource
      ? t("common.mermaid.showDiagram")
      : t("common.mermaid.showSource");
    toggle.title = toggle.textContent;
    toggle.setAttribute("aria-pressed", String(showingSource));
    if (showingSource) {
      wrapper.removeAttribute("data-find-atomic");
      clearAtomicFindText(wrapper);
      pre.removeAttribute("data-find-skip");
    } else {
      wrapper.setAttribute("data-find-atomic", "");
      setAtomicFindText(wrapper, document.findText);
      pre.setAttribute("data-find-skip", "");
    }
    if (notify) announceUpdate(rootHeight() - modeBeforeHeight);
  };
  toggle.addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    showingSource = !showingSource;
    syncMode();
  });

  wrapper.classList.remove("mermaid-loading", "mermaid-error");
  wrapper.classList.add("mermaid-rendered");
  wrapper.removeAttribute("data-mermaid-pending");
  wrapper.setAttribute("role", "group");
  wrapper.setAttribute("aria-label", diagramLabel(document));
  syncMode(false);
  announceUpdate(rootHeight() - beforeHeight);
}

function markFailure(
  wrapper: HTMLElement,
  code: "failed" | "tooLarge" | "tooMany",
  token: number,
): void {
  if (token !== generation || !root.value?.contains(wrapper)) return;
  const beforeHeight = rootHeight();
  wrapper.querySelector(".mermaid-status")?.remove();
  wrapper.classList.remove("mermaid-loading");
  wrapper.classList.add("mermaid-error");
  wrapper.removeAttribute("data-mermaid-pending");
  makeStatus(wrapper, code);
  announceUpdate(rootHeight() - beforeHeight);
}

async function renderBlock(wrapper: HTMLElement, token: number): Promise<void> {
  const pre = Array.from(wrapper.children).find(
    (child): child is HTMLElement =>
      child instanceof HTMLElement && child.localName === "pre",
  );
  const code = pre?.querySelector("code");
  if (!pre || !code) {
    markFailure(wrapper, "failed", token);
    return;
  }
  const source = code.textContent ?? "";
  if (source.length > MERMAID_MAX_TEXT_SIZE) {
    markFailure(wrapper, "tooLarge", token);
    return;
  }

  wrapper.classList.add("mermaid-loading");
  makeStatus(wrapper, "rendering");
  try {
    const renderer = await loadMermaidAdapter();
    const fontSize = diagramFontSize(wrapper);
    const document = await renderer.renderMermaid(
      source,
      effectiveColorScheme.value,
      fontSize,
    );
    installDiagram(wrapper, pre, document, token, fontSize);
  } catch (error) {
    const code =
      typeof error === "object" &&
      error !== null &&
      "code" in error &&
      error.code === "tooLarge"
        ? "tooLarge"
        : "failed";
    markFailure(wrapper, code, token);
  }
}

function scheduleBlock(wrapper: HTMLElement, token: number): void {
  if (!props.lazyMermaid || typeof IntersectionObserver === "undefined") {
    void renderBlock(wrapper, token);
    return;
  }
  if (!observer) {
    observer = new IntersectionObserver(
      (entries) => {
        for (const entry of entries) {
          if (!entry.isIntersecting) continue;
          const target = entry.target as HTMLElement;
          observer?.unobserve(target);
          void renderBlock(target, generation);
        }
      },
      { root: null, rootMargin: "320px 0px" },
    );
  }
  observer.observe(wrapper);
}

async function hydrateMermaid(): Promise<void> {
  const token = ++generation;
  observer?.disconnect();
  observer = null;
  clearDiagramEffects();
  await nextTick();
  const element = root.value;
  if (!mounted.value || !element || token !== generation) return;

  // Theme and locale changes must start from the original Markdown DOM.
  const beforeHeight = rootHeight();
  element.innerHTML = html.value;
  if (!props.enableMermaid) {
    announceUpdate(rootHeight() - beforeHeight);
    return;
  }

  const blocks = Array.from(
    element.querySelectorAll<HTMLElement>(".mermaid-block[data-mermaid-pending]"),
  );
  blocks.forEach((wrapper, index) => {
    if (index >= MERMAID_MAX_DIAGRAMS) {
      markFailure(wrapper, "tooMany", token);
    } else {
      scheduleBlock(wrapper, token);
    }
  });
  announceUpdate(rootHeight() - beforeHeight);
}

function onRootClick(event: MouseEvent): void {
  if (handleCodeCopyClick(event)) return;
  const anchor = (event.target as HTMLElement | null)?.closest?.("a") as
    | HTMLAnchorElement
    | null;
  if (!anchor || !/^(https?:|mailto:)/i.test(anchor.href)) return;
  event.preventDefault();
  event.stopPropagation();
  openPath(anchor.href).catch(() => {});
}

onMounted(() => {
  mounted.value = true;
  void hydrateMermaid();
});

watch(
  [html, effectiveColorScheme, locale, () => props.enableMermaid, () => props.lazyMermaid],
  () => {
    if (mounted.value) void hydrateMermaid();
  },
  { flush: "post" },
);

onBeforeUnmount(() => {
  mounted.value = false;
  generation += 1;
  observer?.disconnect();
  observer = null;
  clearDiagramEffects();
  if (root.value) {
    for (const element of Array.from(root.value.querySelectorAll("[data-find-atomic]"))) {
      clearAtomicFindText(element);
    }
  }
});
</script>

<template>
  <div ref="root" class="markdown-body" v-html="html" @click="onRootClick"></div>
</template>
