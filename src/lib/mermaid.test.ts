import { afterAll, beforeAll, describe, expect, it } from "vitest";
import {
  MERMAID_MAX_EDGES,
  MERMAID_MAX_TEXT_SIZE,
  MermaidRenderError,
  normalizeMermaidSandbox,
  renderMermaid,
} from "./mermaid";
import { mermaidFitScale } from "./mermaidLimits";

function sandbox(svg: string): string {
  const bytes = new TextEncoder().encode(`<body style="margin:0">${svg}</body>`);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  const encoded = btoa(binary);
  return `<iframe src="data:text/html;charset=UTF-8;base64,${encoded}" sandbox="allow-popups"></iframe>`;
}

function decodeDocument(url: string): string {
  return atob(url.split(",", 2)[1]!);
}

function fakeCanvasContext(this: HTMLCanvasElement): object {
  const canvas = this;
  return new Proxy(
    {
      canvas,
      measureText: () => ({ width: 80 }),
      createLinearGradient: () => ({ addColorStop: () => {} }),
      createRadialGradient: () => ({ addColorStop: () => {} }),
      createPattern: () => null,
      getImageData: () => ({ data: new Uint8ClampedArray() }),
    },
    {
      get(target, key) {
        return key in target ? target[key as keyof typeof target] : () => {};
      },
      set(target, key, value) {
        Reflect.set(target, key, value);
        return true;
      },
    },
  );
}

function hideProperty(target: object, key: PropertyKey): () => void {
  const descriptor = Object.getOwnPropertyDescriptor(target, key);
  Object.defineProperty(target, key, {
    configurable: true,
    writable: true,
    value: undefined,
  });
  return () => {
    if (descriptor) Object.defineProperty(target, key, descriptor);
    else Reflect.deleteProperty(target, key);
  };
}

describe("normalizeMermaidSandbox", () => {
  it("repackages static SVG under an owned CSP document", () => {
    const result = normalizeMermaidSandbox(
      sandbox(
        '<svg viewBox="0 0 240 120"><title>Plan</title><desc>A flow</desc><text>开始</text><path marker-end="url(#arrow)"/><marker id="arrow"/></svg>',
      ),
    );

    expect(result.width).toBe(240);
    expect(result.height).toBe(120);
    expect(result.findText).toContain("开始");
    expect(result.accTitle).toBe("Plan");
    const html = decodeDocument(result.documentUrl);
    expect(html).toContain("default-src 'none'");
    expect(html).toContain("connect-src 'none'");
    expect(html).toContain("<svg");
    expect(html).not.toContain("allow-popups");
  });

  it.each([
    '<svg viewBox="0 0 1 1"><script>alert(1)</script></svg>',
    '<svg viewBox="0 0 1 1"><a href="https://example.com"><text>x</text></a></svg>',
    '<svg viewBox="0 0 1 1"><image href="https://example.com/x.png"/></svg>',
    '<svg viewBox="0 0 1 1"><style>.x{fill:url(https://example.com/x)}</style></svg>',
    '<svg viewBox="0 0 1 1"><text onclick="alert(1)">x</text></svg>',
  ])("rejects active or external output", (svg) => {
    expect(() => normalizeMermaidSandbox(sandbox(svg))).toThrow(
      MermaidRenderError,
    );
  });

  it("fails closed when Mermaid changes its wrapper shape", () => {
    expect(() =>
      normalizeMermaidSandbox('<svg viewBox="0 0 1 1"></svg>'),
    ).toThrow("Missing Mermaid sandbox iframe");
    expect(() =>
      normalizeMermaidSandbox(
        sandbox('<svg viewBox="0 0 0 1"><text>x</text></svg>'),
      ),
    ).toThrow("invalid dimensions");
  });
});

describe("mermaidFitScale", () => {
  it("fits ordinary overflow while preserving a 12px visible font floor", () => {
    expect(mermaidFitScale(560, 480, 14)).toBeCloseTo(480 / 560);
    expect(mermaidFitScale(1_200, 480, 14)).toBeCloseTo(12 / 14);
    expect(mermaidFitScale(320, 480, 14)).toBe(1);
  });
});

describe("renderMermaid", () => {
  const nativeAppendChild = Node.prototype.appendChild;
  const nativeGetContext = HTMLCanvasElement.prototype.getContext;

  beforeAll(() => {
    const svgPrototype = SVGElement.prototype as SVGElement & {
      getBBox?: () => DOMRect;
      getComputedTextLength?: () => number;
    };
    if (!svgPrototype.getBBox) {
      svgPrototype.getBBox = () =>
        ({ x: 0, y: 0, width: 80, height: 20 } as DOMRect);
    }
    if (!svgPrototype.getComputedTextLength) {
      svgPrototype.getComputedTextLength = () => 80;
    }
    const htmlPrototype = HTMLElement.prototype as HTMLElement & {
      getComputedTextLength?: () => number;
    };
    if (!htmlPrototype.getComputedTextLength) {
      htmlPrototype.getComputedTextLength = () => 80;
    }
    Object.defineProperty(HTMLCanvasElement.prototype, "getContext", {
      configurable: true,
      value: fakeCanvasContext,
    });

    Node.prototype.appendChild = function <T extends Node>(child: T): T {
      const appended = nativeAppendChild.call(this, child) as T;
      if (child instanceof HTMLIFrameElement && child.contentWindow) {
        const frame = child.contentWindow as unknown as typeof window;
        Object.defineProperty(frame.SVGElement.prototype, "getBBox", {
          configurable: true,
          value: () => ({ x: 0, y: 0, width: 80, height: 20 }),
        });
        Object.defineProperty(frame.Element.prototype, "getComputedTextLength", {
          configurable: true,
          value: () => 80,
        });
        Object.defineProperty(frame.HTMLCanvasElement.prototype, "getContext", {
          configurable: true,
          value: fakeCanvasContext,
        });
      }
      return appended;
    };
  });

  afterAll(() => {
    Node.prototype.appendChild = nativeAppendChild;
    HTMLCanvasElement.prototype.getContext = nativeGetContext;
  });

  it("rejects oversized input before loading the renderer", async () => {
    await expect(
      renderMermaid("x".repeat(MERMAID_MAX_TEXT_SIZE + 1), "light"),
    ).rejects.toMatchObject({ code: "tooLarge" });
  });

  it(
    "enforces the configured edge limit",
    async () => {
      const edges = Array.from(
        { length: MERMAID_MAX_EDGES + 1 },
        (_, index) => `N${index}-->N${index + 1}`,
      ).join("\n");
      await expect(renderMermaid(`flowchart TD\n${edges}`, "light")).rejects
        .toMatchObject({ code: "renderFailed" });
    },
    // Mermaid parses the intentionally oversized graph before rejecting it. Windows CI and
    // constrained VMs can take longer than Vitest's generic 5-second unit-test default.
    15_000,
  );

  it(
    "renders and normalizes a real Mermaid flowchart",
    async () => {
      const result = await renderMermaid(
        "flowchart TD\n  A[开始] --> B[完成]",
        "light",
      );
      expect(result.documentUrl).toMatch(
        /^data:text\/html;charset=UTF-8;base64,/,
      );
      expect(result.findText).toContain("开始");
      expect(result.findText).toContain("完成");
      expect(decodeDocument(result.documentUrl)).toContain("default-src 'none'");
    },
    // The preceding edge-limit case intentionally stresses Mermaid's parser. Its cleanup can
    // leave the next real render slower on Windows CI and constrained VMs.
    15_000,
  );

  it("uses the Markdown body font size in the Mermaid theme", async () => {
    const result = await renderMermaid("flowchart TD\nA[Readable]-->B", "light", 12);
    expect(decodeDocument(result.documentUrl)).toContain("font-size:12px");
  });

  it.each([
    ["sequence", "sequenceDiagram\n  Alice->>Bob: 你好", "你好"],
    ["state", "stateDiagram-v2\n  [*] --> Ready\n  Ready --> Done", "Ready"],
    ["class", "classDiagram\n  class Worker\n  Worker : +run()", "Worker"],
    ["mindmap", "mindmap\n  root((计划))\n    实现\n    验证", "计划"],
  ])("renders a real %s diagram", async (_name, source, label) => {
    const result = await renderMermaid(source, "dark");
    expect(result.findText).toContain(label);
    expect(decodeDocument(result.documentUrl)).toContain("default-src 'none'");
  });

  it("locks security-sensitive settings against diagram directives", async () => {
    const result = await renderMermaid(
      `%%{init: {"securityLevel":"loose","htmlLabels":true,"theme":"forest","maxEdges":99999}}%%
flowchart TD
  A[Safe] --> B[Done]`,
      "light",
    );
    const document = decodeDocument(result.documentUrl);
    expect(document).toContain("default-src 'none'");
    expect(document).not.toMatch(/foreignObject/i);
  });

  it("fails closed when a diagram tries to create an external link", async () => {
    await expect(
      renderMermaid(
        'flowchart TD\n  A[Safe] --> B[Done]\n  click A "https://example.com"',
        "light",
      ),
    ).rejects.toMatchObject({ code: "unsafeOutput" });
  });

  it("uses unique SVG ids across renders", async () => {
    const [first, second] = await Promise.all([
      renderMermaid("flowchart TD\nA-->B", "light"),
      renderMermaid("flowchart TD\nA-->B", "light"),
    ]);
    const firstId = decodeDocument(first.documentUrl).match(/<svg[^>]*\sid="([^"]+)"/)?.[1];
    const secondId = decodeDocument(second.documentUrl).match(/<svg[^>]*\sid="([^"]+)"/)?.[1];
    expect(firstId).toBeTruthy();
    expect(secondId).toBeTruthy();
    expect(firstId).not.toBe(secondId);
  });

  it("normalizes parser failures without leaving an error diagram", async () => {
    await expect(renderMermaid("this is not mermaid", "light")).rejects.toMatchObject({
      code: "renderFailed",
    });
    expect(document.querySelector('[id^="askhuman-mermaid-"]')).toBeNull();
  });

  it("supplies the ES runtime features missing from Safari 13", async () => {
    const restores = [
      hideProperty(Promise, "allSettled"),
      hideProperty(Object, "hasOwn"),
      hideProperty(Array.prototype, "at"),
      hideProperty(String.prototype, "replaceAll"),
      hideProperty(globalThis, "structuredClone"),
    ];
    try {
      const flow = await renderMermaid(
        '%%{init: {"theme":"forest"}}%%\nflowchart TD\nA-->B',
        "light",
      );
      const mindmap = await renderMermaid(
        "mindmap\n  root((兼容))\n    Safari 13",
        "light",
      );
      expect(flow.documentUrl).toContain("base64,");
      expect(mindmap.findText).toContain("兼容");
    } finally {
      for (let index = restores.length - 1; index >= 0; index -= 1) {
        restores[index]!();
      }
    }
  });

  it("renders when CSSStyleSheet exists but is not constructable", async () => {
    const descriptor = Object.getOwnPropertyDescriptor(globalThis, "CSSStyleSheet");
    const NonConstructable = (() => {
      throw new TypeError("Illegal constructor");
    }) as unknown as typeof CSSStyleSheet;
    Object.defineProperty(globalThis, "CSSStyleSheet", {
      configurable: true,
      writable: true,
      value: NonConstructable,
    });
    try {
      const result = await renderMermaid("flowchart TD\nA-->B", "light");
      expect(result.documentUrl).toContain("base64,");
      expect(globalThis.CSSStyleSheet).toBe(NonConstructable);
    } finally {
      if (descriptor) Object.defineProperty(globalThis, "CSSStyleSheet", descriptor);
      else Reflect.deleteProperty(globalThis, "CSSStyleSheet");
    }
  });
});
