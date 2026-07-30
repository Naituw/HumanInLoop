// In-page find helpers: substring ranges + non-destructive mark wrap/unwrap on text nodes.

export interface TextRange {
  start: number;
  end: number;
}

const MARK_CLASS = "popup-find-hit";
const MARK_CURRENT = "popup-find-hit-current";
const MARK_ATTR = "data-popup-find";

/** All non-overlapping substring ranges of `query` in `text` (left-to-right). */
export function findAllRanges(
  text: string,
  query: string,
  caseSensitive: boolean,
): TextRange[] {
  if (!query) return [];
  const hay = caseSensitive ? text : text.toLowerCase();
  const needle = caseSensitive ? query : query.toLowerCase();
  if (!needle) return [];
  const out: TextRange[] = [];
  let from = 0;
  while (from <= hay.length - needle.length) {
    const idx = hay.indexOf(needle, from);
    if (idx < 0) break;
    out.push({ start: idx, end: idx + needle.length });
    from = idx + Math.max(1, needle.length);
  }
  return out;
}

export function isFindMark(el: Node): el is HTMLElement {
  return (
    el.nodeType === Node.ELEMENT_NODE &&
    (el as HTMLElement).hasAttribute(MARK_ATTR)
  );
}

/** Plain text under `root` using the same skip rules as applyFindMarks. */
export function collectFindableText(root: HTMLElement): string {
  const parts: string[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent) return NodeFilter.FILTER_REJECT;
      if (parent.closest("script, style, textarea, input, [data-find-skip]")) {
        return NodeFilter.FILTER_REJECT;
      }
      if (!node.nodeValue) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  let n = walker.nextNode();
  while (n) {
    parts.push(n.nodeValue ?? "");
    n = walker.nextNode();
  }
  return parts.join("");
}

/** Remove all find marks under `root`, restoring plain text nodes where possible. */
export function clearFindMarks(root: ParentNode): void {
  const marks = root.querySelectorAll(`[${MARK_ATTR}]`);
  for (const mark of Array.from(marks)) {
    const parent = mark.parentNode;
    if (!parent) continue;
    while (mark.firstChild) parent.insertBefore(mark.firstChild, mark);
    parent.removeChild(mark);
    parent.normalize();
  }
}

/**
 * Wrap every occurrence of `query` in text nodes under `root`.
 * Returns mark elements in document order.
 * Skips script/style and elements marked data-find-skip.
 */
export function applyFindMarks(
  root: HTMLElement,
  query: string,
  caseSensitive: boolean,
): HTMLElement[] {
  clearFindMarks(root);
  if (!query) return [];

  const marks: HTMLElement[] = [];
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent) return NodeFilter.FILTER_REJECT;
      if (parent.closest("script, style, textarea, input, [data-find-skip]")) {
        return NodeFilter.FILTER_REJECT;
      }
      if (!node.nodeValue) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });

  // Collect first — mutating while walking breaks the walker.
  const texts: Text[] = [];
  let n = walker.nextNode();
  while (n) {
    texts.push(n as Text);
    n = walker.nextNode();
  }

  for (const textNode of texts) {
    const value = textNode.nodeValue ?? "";
    const ranges = findAllRanges(value, query, caseSensitive);
    if (ranges.length === 0) continue;

    // Split from the end so earlier offsets stay valid; collect in document order.
    const nodeMarks: HTMLElement[] = [];
    for (let i = ranges.length - 1; i >= 0; i--) {
      const { start, end } = ranges[i]!;
      const full = textNode.nodeValue ?? "";
      if (start < 0 || end > full.length || start >= end) continue;

      textNode.splitText(end);
      const mid = textNode.splitText(start);
      const mark = document.createElement("mark");
      mark.className = MARK_CLASS;
      mark.setAttribute(MARK_ATTR, "1");
      mid.parentNode?.replaceChild(mark, mid);
      mark.appendChild(mid);
      nodeMarks.unshift(mark);
    }
    marks.push(...nodeMarks);
  }

  return marks;
}

export function setCurrentFindMark(
  marks: HTMLElement[],
  currentIndex: number,
): HTMLElement | null {
  let current: HTMLElement | null = null;
  for (let i = 0; i < marks.length; i++) {
    const el = marks[i]!;
    if (i === currentIndex) {
      el.classList.add(MARK_CURRENT);
      current = el;
    } else {
      el.classList.remove(MARK_CURRENT);
    }
  }
  return current;
}

export { MARK_CLASS, MARK_CURRENT, MARK_ATTR };
