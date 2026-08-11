import type { MermaidConfig } from "mermaid";
import {
  MERMAID_MAX_EDGES,
  MERMAID_MAX_TEXT_SIZE,
} from "./mermaidLimits";

export {
  MERMAID_MAX_DIAGRAMS,
  MERMAID_MAX_EDGES,
  MERMAID_MAX_TEXT_SIZE,
} from "./mermaidLimits";

const MAX_RENDER_DIMENSION = 30_000;
const SANDBOX_DATA_PREFIX = "data:text/html;charset=UTF-8;base64,";
const SANDBOX_CSP =
  "default-src 'none'; style-src 'unsafe-inline'; img-src 'none'; " +
  "font-src 'none'; connect-src 'none'; media-src 'none'; object-src 'none'; " +
  "script-src 'none'; base-uri 'none'; form-action 'none'";

const FORBIDDEN_ELEMENTS = new Set([
  "a",
  "animate",
  "animatemotion",
  "animatetransform",
  "audio",
  "embed",
  "foreignobject",
  "iframe",
  "image",
  "link",
  "meta",
  "object",
  "script",
  "set",
  "source",
  "video",
]);

const SECURE_CONFIG_KEYS = [
  "secure",
  "securityLevel",
  "startOnLoad",
  "maxTextSize",
  "maxEdges",
  "theme",
  "themeVariables",
  "themeCSS",
  "fontFamily",
  "altFontFamily",
  "htmlLabels",
  "dompurifyConfig",
  "deterministicIds",
  "deterministicIDSeed",
  "markdownAutoWrap",
];

export type MermaidColorScheme = "light" | "dark";

export type MermaidErrorCode =
  | "tooLarge"
  | "loadFailed"
  | "renderFailed"
  | "unsafeOutput"
  | "unsupported";

export class MermaidRenderError extends Error {
  constructor(
    public readonly code: MermaidErrorCode,
    message: string,
    options?: { cause?: unknown },
  ) {
    super(message);
    this.name = "MermaidRenderError";
    if (options && "cause" in options) {
      Object.defineProperty(this, "cause", {
        configurable: true,
        value: options.cause,
      });
    }
  }
}

export interface MermaidDocument {
  documentUrl: string;
  width: number;
  height: number;
  findText: string;
  accTitle: string;
  accDescription: string;
}

type MermaidApi = (typeof import("mermaid"))["default"];

let mermaidPromise: Promise<MermaidApi> | null = null;
let renderQueue: Promise<void> = Promise.resolve();
let renderCounter = 0;
let idPrefix = "";

type MutableRecord = Record<PropertyKey, unknown>;

function installFallback(
  target: MutableRecord,
  key: PropertyKey,
  value: unknown,
  restores: Array<() => void>,
): void {
  if (typeof Reflect.get(target, key) === "function") return;
  const descriptor = Object.getOwnPropertyDescriptor(target, key);
  Object.defineProperty(target, key, {
    configurable: true,
    writable: true,
    value,
  });
  restores.push(() => {
    if (descriptor) Object.defineProperty(target, key, descriptor);
    else Reflect.deleteProperty(target, key);
  });
}

function legacyStructuredClone<T>(input: T, seen = new WeakMap<object, unknown>()): T {
  if (typeof input !== "object" || input === null) return input;
  const known = seen.get(input);
  if (known) return known as T;
  if (input instanceof Date) return new Date(input.getTime()) as T;
  if (input instanceof RegExp) return new RegExp(input.source, input.flags) as T;
  if (input instanceof ArrayBuffer) return input.slice(0) as T;
  if (input instanceof Map) {
    const clone = new Map();
    seen.set(input, clone);
    for (const [key, value] of input) {
      clone.set(legacyStructuredClone(key, seen), legacyStructuredClone(value, seen));
    }
    return clone as T;
  }
  if (input instanceof Set) {
    const clone = new Set();
    seen.set(input, clone);
    for (const value of input) clone.add(legacyStructuredClone(value, seen));
    return clone as T;
  }
  if (ArrayBuffer.isView(input)) {
    if (input instanceof DataView) {
      return new DataView(input.buffer.slice(0), input.byteOffset, input.byteLength) as T;
    }
    const View = input.constructor as new (value: ArrayLike<number>) => T;
    return new View(input as unknown as ArrayLike<number>);
  }

  const clone: MutableRecord = Array.isArray(input)
    ? []
    : Object.create(Object.getPrototypeOf(input));
  seen.set(input, clone);
  for (const key of Reflect.ownKeys(input)) {
    clone[key] = legacyStructuredClone(
      (input as unknown as MutableRecord)[key],
      seen,
    );
  }
  return clone as T;
}

/** Install only the ES built-ins Mermaid 11 uses beyond the Safari 13 baseline. */
function installLegacyRuntimeCompatibility(): () => void {
  const restores: Array<() => void> = [];

  installFallback(
    Promise as unknown as MutableRecord,
    "allSettled",
    (values: Iterable<unknown>) =>
      Promise.all(
        Array.from(values, (value) =>
          Promise.resolve(value).then(
            (resolved) => ({ status: "fulfilled", value: resolved }),
            (reason) => ({ status: "rejected", reason }),
          ),
        ),
      ),
    restores,
  );
  installFallback(
    Object as unknown as MutableRecord,
    "hasOwn",
    (value: object, key: PropertyKey) =>
      Object.prototype.hasOwnProperty.call(value, key),
    restores,
  );
  installFallback(
    Array.prototype as unknown as MutableRecord,
    "at",
    function at(this: unknown[], index: number): unknown {
      const integer = Math.trunc(index) || 0;
      const resolved = integer < 0 ? this.length + integer : integer;
      return resolved < 0 || resolved >= this.length ? undefined : this[resolved];
    },
    restores,
  );
  installFallback(
    String.prototype as unknown as MutableRecord,
    "replaceAll",
    function replaceAll(
      this: string,
      search: string | RegExp,
      replacement: string,
    ): string {
      const value = String(this);
      if (search instanceof RegExp) {
        if (!search.global) throw new TypeError("replaceAll RegExp must be global");
        return value.replace(search, replacement);
      }
      if (search === "") return replacement + value.split("").join(replacement) + replacement;
      return value.split(search).join(replacement);
    },
    restores,
  );
  installFallback(
    globalThis as unknown as MutableRecord,
    "structuredClone",
    legacyStructuredClone,
    restores,
  );

  return () => {
    for (let index = restores.length - 1; index >= 0; index -= 1) {
      restores[index]!();
    }
  };
}

function loadMermaid(): Promise<MermaidApi> {
  if (!mermaidPromise) {
    mermaidPromise = import("mermaid")
      .then((module) => module.default)
      .catch((error) => {
        mermaidPromise = null;
        throw new MermaidRenderError(
          "loadFailed",
          "Failed to load the Mermaid renderer",
          { cause: error },
        );
      });
  }
  return mermaidPromise;
}

function utf8ToBase64(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  const chunkSize = 0x8000;
  for (let i = 0; i < bytes.length; i += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunkSize));
  }
  return btoa(binary);
}

function base64ToUtf8(value: string): string {
  let binary: string;
  try {
    binary = atob(value);
  } catch (error) {
    throw new MermaidRenderError("unsafeOutput", "Invalid Mermaid sandbox data", {
      cause: error,
    });
  }
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i += 1) {
    bytes[i] = binary.charCodeAt(i);
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch (error) {
    throw new MermaidRenderError("unsafeOutput", "Invalid Mermaid sandbox UTF-8", {
      cause: error,
    });
  }
}

function isLocalFragment(value: string): boolean {
  return /^#[A-Za-z0-9_.:-]+$/.test(value.trim());
}

function hasUnsafeCssUrl(value: string): boolean {
  let unsafe = false;
  const withoutUrls = value.replace(
    /url\(\s*(['"]?)(.*?)\1\s*\)/gi,
    (_match, _quote: string, target: string) => {
      if (!isLocalFragment(target)) unsafe = true;
      return "";
    },
  );
  return unsafe || /url\s*\(/i.test(withoutUrls);
}

function validateStaticSvg(svg: SVGElement): void {
  for (const element of [svg, ...Array.from(svg.querySelectorAll("*"))]) {
    const tag = element.localName.toLowerCase();
    if (FORBIDDEN_ELEMENTS.has(tag)) {
      throw new MermaidRenderError(
        "unsafeOutput",
        `Mermaid output contains forbidden <${tag}>`,
      );
    }

    if (tag === "style") {
      const css = element.textContent ?? "";
      if (
        /@import|javascript\s*:|vbscript\s*:|expression\s*\(/i.test(css) ||
        hasUnsafeCssUrl(css)
      ) {
        throw new MermaidRenderError(
          "unsafeOutput",
          "Mermaid output contains unsafe CSS",
        );
      }
    }

    for (const attribute of Array.from(element.attributes)) {
      const name = attribute.name.toLowerCase();
      const value = attribute.value.trim();
      if (name.startsWith("on") || name === "src" || name === "srcset") {
        throw new MermaidRenderError(
          "unsafeOutput",
          `Mermaid output contains forbidden ${name}`,
        );
      }
      if (name === "href" || name === "xlink:href") {
        if (!isLocalFragment(value)) {
          throw new MermaidRenderError(
            "unsafeOutput",
            "Mermaid output contains an external reference",
          );
        }
      }
      if (
        /javascript\s*:|vbscript\s*:|expression\s*\(|behavior\s*:|@import/i.test(
          value,
        ) ||
        (/url\s*\(/i.test(value) && hasUnsafeCssUrl(value))
      ) {
        throw new MermaidRenderError(
          "unsafeOutput",
          "Mermaid output contains an unsafe attribute",
        );
      }
    }
  }
}

function directChildText(svg: SVGElement, name: "title" | "desc"): string {
  const child = Array.from(svg.children).find(
    (candidate) => candidate.localName.toLowerCase() === name,
  );
  return child?.textContent?.trim() ?? "";
}

function visibleSvgText(svg: SVGElement): string {
  const parts = Array.from(svg.querySelectorAll("title, desc, text"))
    .map((element) => element.textContent?.replace(/\s+/g, " ").trim() ?? "")
    .filter(Boolean);
  return Array.from(new Set(parts)).join("\n");
}

function parseViewBox(svg: SVGElement): { width: number; height: number } {
  const values = (svg.getAttribute("viewBox") ?? "")
    .trim()
    .split(/[\s,]+/)
    .map(Number);
  let width = values.length === 4 ? values[2] : Number.NaN;
  let height = values.length === 4 ? values[3] : Number.NaN;

  if (!Number.isFinite(width) || width <= 0) {
    width = Number.parseFloat(svg.getAttribute("width") ?? "");
  }
  if (!Number.isFinite(height) || height <= 0) {
    height = Number.parseFloat(svg.getAttribute("height") ?? "");
  }
  if (
    !Number.isFinite(width) ||
    !Number.isFinite(height) ||
    width <= 0 ||
    height <= 0 ||
    width > MAX_RENDER_DIMENSION ||
    height > MAX_RENDER_DIMENSION
  ) {
    throw new MermaidRenderError(
      "unsafeOutput",
      "Mermaid output has invalid dimensions",
    );
  }
  return { width, height };
}

/**
 * Convert Mermaid's sandbox iframe string into a no-permission, CSP-locked document.
 * This intentionally fails closed if Mermaid changes the wrapper shape.
 */
export function normalizeMermaidSandbox(markup: string): MermaidDocument {
  const outer = new DOMParser().parseFromString(markup, "text/html");
  if (outer.body.children.length !== 1) {
    throw new MermaidRenderError("unsafeOutput", "Unexpected Mermaid sandbox wrapper");
  }
  const iframe = outer.body.firstElementChild;
  if (!iframe || iframe.localName.toLowerCase() !== "iframe") {
    throw new MermaidRenderError("unsafeOutput", "Missing Mermaid sandbox iframe");
  }
  const src = iframe.getAttribute("src") ?? "";
  if (!src.startsWith(SANDBOX_DATA_PREFIX)) {
    throw new MermaidRenderError("unsafeOutput", "Unexpected Mermaid sandbox URL");
  }

  const decoded = base64ToUtf8(src.slice(SANDBOX_DATA_PREFIX.length));
  const inner = new DOMParser().parseFromString(decoded, "text/html");
  if (inner.body.children.length !== 1) {
    throw new MermaidRenderError("unsafeOutput", "Unexpected Mermaid sandbox document");
  }
  const svg = inner.body.firstElementChild;
  if (!svg || svg.localName.toLowerCase() !== "svg") {
    throw new MermaidRenderError("unsafeOutput", "Missing Mermaid SVG");
  }

  const svgElement = svg as unknown as SVGElement;
  validateStaticSvg(svgElement);
  const { width, height } = parseViewBox(svgElement);
  const accTitle = directChildText(svgElement, "title");
  const accDescription = directChildText(svgElement, "desc");
  const findText = visibleSvgText(svgElement);
  const serialized = new XMLSerializer().serializeToString(svgElement);
  const lockedDocument =
    "<!doctype html><html><head><meta charset=\"utf-8\">" +
    `<meta http-equiv="Content-Security-Policy" content="${SANDBOX_CSP}">` +
    "<style>html,body{margin:0;padding:0;background:transparent;overflow:hidden}" +
    "svg{display:block}</style></head><body>" +
    serialized +
    "</body></html>";

  return {
    documentUrl: SANDBOX_DATA_PREFIX + utf8ToBase64(lockedDocument),
    width,
    height,
    findText,
    accTitle,
    accDescription,
  };
}

const PALETTES: Record<MermaidColorScheme, Record<string, string | boolean>> = {
  light: {
    darkMode: false,
    background: "#f0f0f2",
    primaryColor: "#e8f2ff",
    primaryTextColor: "#1d1d1f",
    primaryBorderColor: "#7aaee8",
    secondaryColor: "#eef0f3",
    secondaryTextColor: "#1d1d1f",
    secondaryBorderColor: "#a9adb4",
    tertiaryColor: "#fff4d6",
    tertiaryTextColor: "#1d1d1f",
    tertiaryBorderColor: "#d6b35f",
    lineColor: "#5f6368",
    textColor: "#1d1d1f",
    noteBkgColor: "#fff4c2",
    noteTextColor: "#1d1d1f",
    noteBorderColor: "#d6b35f",
  },
  dark: {
    darkMode: true,
    background: "#1e1e1e",
    primaryColor: "#173553",
    primaryTextColor: "#f2f2f4",
    primaryBorderColor: "#4ea1ff",
    secondaryColor: "#34363a",
    secondaryTextColor: "#f2f2f4",
    secondaryBorderColor: "#777b82",
    tertiaryColor: "#4a3b16",
    tertiaryTextColor: "#f2f2f4",
    tertiaryBorderColor: "#c6a34a",
    lineColor: "#b8bbc1",
    textColor: "#f2f2f4",
    noteBkgColor: "#4a3b16",
    noteTextColor: "#f2f2f4",
    noteBorderColor: "#c6a34a",
  },
};

function mermaidConfig(
  theme: MermaidColorScheme,
  seed: string,
  fontSize: number,
): MermaidConfig {
  return {
    startOnLoad: false,
    securityLevel: "sandbox",
    theme: "base",
    themeVariables: {
      ...PALETTES[theme],
      fontSize: `${fontSize}px`,
    },
    htmlLabels: false,
    markdownAutoWrap: false,
    maxTextSize: MERMAID_MAX_TEXT_SIZE,
    maxEdges: MERMAID_MAX_EDGES,
    suppressErrorRendering: true,
    logLevel: "fatal",
    deterministicIds: true,
    deterministicIDSeed: seed,
    fontFamily:
      '-apple-system, "SF Pro Text", system-ui, "Segoe UI", "PingFang SC", sans-serif',
    secure: SECURE_CONFIG_KEYS,
    dompurifyConfig: {
      FORBID_TAGS: ["a", "foreignObject", "image", "script"],
      FORBID_ATTR: ["href", "xlink:href", "src", "onload", "onclick"],
    },
  };
}

function nextRenderId(): string {
  if (!idPrefix) {
    const random = new Uint32Array(2);
    if (globalThis.crypto?.getRandomValues) {
      globalThis.crypto.getRandomValues(random);
      idPrefix = `${random[0].toString(36)}${random[1].toString(36)}`;
    } else {
      idPrefix = Math.random().toString(36).slice(2, 12);
    }
  }
  renderCounter += 1;
  return `askhuman-mermaid-${idPrefix}-${renderCounter}`;
}

/**
 * Mermaid 11.15+ uses `new CSSStyleSheet()` while AskHuman still supports Safari 13,
 * where CSSStyleSheet may exist but not be constructable. Use a native <style>.sheet
 * as a short-lived CSSOM-compatible constructor during the serialized render.
 */
function installStyleSheetCompatibility(): () => void {
  const globalObject = globalThis as typeof globalThis & {
    CSSStyleSheet?: typeof CSSStyleSheet;
  };
  const Native = globalObject.CSSStyleSheet;
  if (Native) {
    try {
      new Native();
      return () => {};
    } catch {
      // Fall through to the native style-element backed shim.
    }
  }

  const descriptor = Object.getOwnPropertyDescriptor(globalObject, "CSSStyleSheet");
  if (descriptor && !descriptor.configurable) {
    throw new MermaidRenderError(
      "unsupported",
      "This WebView cannot construct a CSS stylesheet",
    );
  }

  const styleElements: HTMLStyleElement[] = [];
  const CompatibleStyleSheet = function CompatibleStyleSheet(): CSSStyleSheet {
    const style = document.createElement("style");
    style.setAttribute("data-mermaid-cssom-shim", "");
    document.head.appendChild(style);
    const sheet = style.sheet;
    if (!sheet) {
      style.remove();
      throw new MermaidRenderError(
        "unsupported",
        "This WebView cannot create a CSS stylesheet",
      );
    }
    styleElements.push(style);
    return sheet;
  } as unknown as typeof CSSStyleSheet;

  Object.defineProperty(globalObject, "CSSStyleSheet", {
    configurable: true,
    writable: true,
    value: CompatibleStyleSheet,
  });

  return () => {
    for (const style of styleElements) style.remove();
    if (descriptor) {
      Object.defineProperty(globalObject, "CSSStyleSheet", descriptor);
    } else {
      Reflect.deleteProperty(globalObject, "CSSStyleSheet");
    }
  };
}

async function renderInternal(
  source: string,
  theme: MermaidColorScheme,
  fontSize: number,
): Promise<MermaidDocument> {
  if (source.length > MERMAID_MAX_TEXT_SIZE) {
    throw new MermaidRenderError(
      "tooLarge",
      `Mermaid source exceeds ${MERMAID_MAX_TEXT_SIZE} characters`,
    );
  }

  const restoreRuntime = installLegacyRuntimeCompatibility();
  try {
    const mermaid = await loadMermaid();
    const renderId = nextRenderId();
    const restoreStyleSheet = installStyleSheetCompatibility();
    try {
      mermaid.initialize(mermaidConfig(theme, renderId, fontSize));
      const result = await mermaid.render(renderId, source);
      return normalizeMermaidSandbox(result.svg);
    } finally {
      restoreStyleSheet();
    }
  } catch (error) {
    if (error instanceof MermaidRenderError) throw error;
    throw new MermaidRenderError("renderFailed", "Failed to render Mermaid", {
      cause: error,
    });
  } finally {
    restoreRuntime();
  }
}

export function renderMermaid(
  source: string,
  theme: MermaidColorScheme,
  fontSize = 14,
): Promise<MermaidDocument> {
  const normalizedFontSize = Number.isFinite(fontSize)
    ? Math.min(24, Math.max(10, fontSize))
    : 14;
  const job = renderQueue.then(() =>
    renderInternal(source, theme, normalizedFontSize),
  );
  renderQueue = job.then(
    () => undefined,
    () => undefined,
  );
  return job;
}
