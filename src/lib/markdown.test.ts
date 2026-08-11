import { beforeAll, describe, expect, it, vi } from "vitest";
import { handleCodeCopyClick, markdownReady, renderMarkdown } from "./markdown";

// The renderer is loaded lazily (R4 bundle split); tests exercise the real one.
beforeAll(() => markdownReady);

describe("renderMarkdown", () => {
  it("escapes raw HTML (html: false)", () => {
    const html = renderMarkdown('<img src=x onerror=alert(1)> **bold**');
    expect(html).not.toContain("<img");
    expect(html).toContain("&lt;img");
    expect(html).toContain("<strong>bold</strong>");
  });

  it("linkifies bare URLs", () => {
    expect(renderMarkdown("see https://example.com")).toContain(
      '<a href="https://example.com">'
    );
  });

  it("wraps fenced code blocks with a localized copy button", () => {
    const html = renderMarkdown("```\nlet x = 1;\n```", {
      copyLabel: "复制",
      copiedLabel: "已复制",
    });
    expect(html).toContain('<div class="code-block">');
    expect(html).toContain('data-copy="复制"');
    expect(html).toContain('data-copied="已复制"');
  });

  it("escapes HTML in the copy labels", () => {
    const html = renderMarkdown("```\nx\n```", { copyLabel: '"><script>' });
    expect(html).not.toContain('data-copy=""><script>');
  });

  it("marks Mermaid fences without changing ordinary fences", () => {
    const mermaid = renderMarkdown("``` Mermaid extra\ngraph TD; A-->B\n```");
    const ordinary = renderMarkdown("```typescript\nconst x = 1\n```");

    expect(mermaid).toContain('class="code-block mermaid-block"');
    expect(mermaid).toContain("data-mermaid-pending");
    expect(mermaid).toContain("graph TD; A--&gt;B");
    expect(ordinary).toContain('<div class="code-block">');
    expect(ordinary).not.toContain("data-mermaid-pending");
  });

  it("only treats the first normalized info token as Mermaid", () => {
    expect(renderMarkdown("```not-mermaid mermaid\nx\n```")).not.toContain(
      "data-mermaid-pending",
    );
    expect(renderMarkdown("```mermaidish\nx\n```")).not.toContain(
      "data-mermaid-pending",
    );
  });
});

describe("handleCodeCopyClick", () => {
  function renderIntoDom(markdown: string): HTMLElement {
    const host = document.createElement("div");
    host.innerHTML = renderMarkdown(markdown);
    document.body.appendChild(host);
    return host;
  }

  it("copies the sibling code text when the copy button is clicked", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });

    const host = renderIntoDom("```\nlet x = 1;\n```");
    const btn = host.querySelector(".code-copy")!;
    const event = new MouseEvent("click", { bubbles: true });
    Object.defineProperty(event, "target", { value: btn });

    expect(handleCodeCopyClick(event)).toBe(true);
    expect(writeText).toHaveBeenCalledWith("let x = 1;\n");
    host.remove();
  });

  it("ignores clicks outside a copy button", () => {
    const host = renderIntoDom("plain paragraph");
    const p = host.querySelector("p")!;
    const event = new MouseEvent("click", { bubbles: true });
    Object.defineProperty(event, "target", { value: p });

    expect(handleCodeCopyClick(event)).toBe(false);
    host.remove();
  });
});
