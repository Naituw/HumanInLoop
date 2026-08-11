// In-page find helpers: substring ranges + non-destructive mark wrap/unwrap on text nodes.

export interface TextRange {
  start: number;
  end: number;
}

const MARK_CLASS = "popup-find-hit";
const MARK_CURRENT = "popup-find-hit-current";
const MARK_ATTR = "data-popup-find";
const ATOMIC_ATTR = "data-find-atomic";
const ATOMIC_PROXY_ATTR = "data-popup-find-atomic-proxy";
const atomicFindText = new WeakMap<Element, string>();

export function setAtomicFindText(element: Element, text: string): void {
  atomicFindText.set(element, text);
}

export function clearAtomicFindText(element: Element): void {
  atomicFindText.delete(element);
}

function atomicText(element: Element): string {
  return atomicFindText.get(element) ?? element.getAttribute("aria-label") ?? "";
}

function isSkippedElement(element: Element): boolean {
  return element.matches("script, style, textarea, input, [data-find-skip]");
}

type FindCandidate =
  | { kind: "text"; node: Text }
  | { kind: "atomic"; element: HTMLElement; text: string };

function collectCandidates(root: HTMLElement): FindCandidate[] {
  const candidates: FindCandidate[] = [];
  const visit = (node: Node): void => {
    if (node.nodeType === Node.TEXT_NODE) {
      if (node.nodeValue) candidates.push({ kind: "text", node: node as Text });
      return;
    }
    if (node.nodeType !== Node.ELEMENT_NODE) return;
    const element = node as HTMLElement;
    if (element !== root && isSkippedElement(element)) return;
    if (element !== root && element.hasAttribute(ATOMIC_ATTR)) {
      candidates.push({ kind: "atomic", element, text: atomicText(element) });
      return;
    }
    for (const child of Array.from(element.childNodes)) visit(child);
  };
  visit(root);
  return candidates;
}

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
  return collectCandidates(root)
    .map((candidate) =>
      candidate.kind === "text" ? candidate.node.nodeValue ?? "" : candidate.text,
    )
    .join("");
}

/**
 * Find ranges that can actually be highlighted in the current DOM. Atomic units
 * contribute at most one match, even when the label contains the query repeatedly.
 */
export function findHighlightableRanges(
  root: HTMLElement,
  query: string,
  caseSensitive: boolean,
): TextRange[] {
  const ranges: TextRange[] = [];
  let offset = 0;
  for (const candidate of collectCandidates(root)) {
    const text =
      candidate.kind === "text" ? candidate.node.nodeValue ?? "" : candidate.text;
    const local = findAllRanges(text, query, caseSensitive);
    if (candidate.kind === "atomic") {
      const first = local[0];
      if (first) {
        ranges.push({ start: offset + first.start, end: offset + first.end });
      }
    } else {
      ranges.push(
        ...local.map((range) => ({
          start: offset + range.start,
          end: offset + range.end,
        })),
      );
    }
    offset += text.length;
  }
  return ranges;
}

/** Remove all find marks under `root`, restoring plain text nodes where possible. */
export function clearFindMarks(root: ParentNode): void {
  const atomicProxies = root.querySelectorAll(`[${ATOMIC_PROXY_ATTR}]`);
  for (const proxy of Array.from(atomicProxies)) {
    proxy.parentElement?.classList.remove(
      "popup-find-atomic-hit",
      "popup-find-atomic-current",
    );
    proxy.remove();
  }

  const marks = root.querySelectorAll(`[${MARK_ATTR}]:not([${ATOMIC_PROXY_ATTR}])`);
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
  // Collect first — mutating while walking breaks traversal.
  const candidates = collectCandidates(root);

  for (const candidate of candidates) {
    if (candidate.kind === "atomic") {
      const ranges = findAllRanges(candidate.text, query, caseSensitive);
      if (ranges.length === 0) continue;
      candidate.element.classList.add("popup-find-atomic-hit");
      const proxy = document.createElement("span");
      proxy.className = `${MARK_CLASS} popup-find-atomic-proxy`;
      proxy.setAttribute(MARK_ATTR, "1");
      proxy.setAttribute(ATOMIC_PROXY_ATTR, "1");
      proxy.setAttribute("aria-hidden", "true");
      candidate.element.appendChild(proxy);
      marks.push(proxy);
      continue;
    }

    const textNode = candidate.node;
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
  const atomicRoots = new Set<HTMLElement>();
  for (const el of marks) {
    const root = el.closest<HTMLElement>(`[${ATOMIC_ATTR}]`);
    if (root) atomicRoots.add(root);
  }
  for (const root of atomicRoots) root.classList.remove("popup-find-atomic-current");
  for (let i = 0; i < marks.length; i++) {
    const el = marks[i]!;
    if (i === currentIndex) {
      el.classList.add(MARK_CURRENT);
      el.closest<HTMLElement>(`[${ATOMIC_ATTR}]`)?.classList.add(
        "popup-find-atomic-current",
      );
      current = el;
    } else {
      el.classList.remove(MARK_CURRENT);
    }
  }
  return current;
}

export { MARK_CLASS, MARK_CURRENT, MARK_ATTR };
