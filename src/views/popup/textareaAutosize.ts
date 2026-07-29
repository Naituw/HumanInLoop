export const DEFAULT_TEXTAREA_MAX_HEIGHT = 240;

const MIRROR_STYLE_PROPERTIES = [
  "boxSizing",
  "width",
  "paddingTop",
  "paddingRight",
  "paddingBottom",
  "paddingLeft",
  "borderTopWidth",
  "borderRightWidth",
  "borderBottomWidth",
  "borderLeftWidth",
  "fontFamily",
  "fontSize",
  "fontStyle",
  "fontWeight",
  "fontVariant",
  "lineHeight",
  "letterSpacing",
  "textIndent",
  "textTransform",
  "whiteSpace",
  "wordBreak",
  "overflowWrap",
  "tabSize",
  "writingMode",
] as const;

function px(value: string): number {
  const parsed = Number.parseFloat(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function heightFromScrollHeight(
  scrollHeight: number,
  style: CSSStyleDeclaration
): number {
  const borderHeight =
    style.boxSizing === "border-box"
      ? px(style.borderTopWidth) + px(style.borderBottomWidth)
      : 0;
  const minHeight = px(style.minHeight);
  return Math.max(minHeight, scrollHeight + borderHeight);
}

function measureTextareaContentHeight(
  textarea: HTMLTextAreaElement,
  style: CSSStyleDeclaration
): number {
  const mirror = textarea.cloneNode(false) as HTMLTextAreaElement;
  mirror.value = textarea.value;
  mirror.removeAttribute("id");
  mirror.removeAttribute("name");
  mirror.setAttribute("aria-hidden", "true");
  mirror.tabIndex = -1;

  for (const property of MIRROR_STYLE_PROPERTIES) {
    mirror.style[property] = style[property];
  }
  mirror.style.position = "fixed";
  mirror.style.top = "-10000px";
  mirror.style.left = "0";
  mirror.style.height = "0";
  mirror.style.minHeight = "0";
  mirror.style.maxHeight = "none";
  mirror.style.overflow = "hidden";
  mirror.style.visibility = "hidden";
  mirror.style.pointerEvents = "none";
  mirror.style.transition = "none";
  mirror.style.resize = "none";

  document.body.appendChild(mirror);
  const measured = heightFromScrollHeight(mirror.scrollHeight, style);
  mirror.remove();
  return measured;
}

export function resizeTextareaToContent(
  textarea: HTMLTextAreaElement | null,
  expanded: boolean,
  allowShrink = true,
  maxHeight = DEFAULT_TEXTAREA_MAX_HEIGHT
) {
  if (!textarea) return;
  if (!expanded) {
    textarea.style.height = "";
    return;
  }

  const style = getComputedStyle(textarea);
  const currentHeight = textarea.getBoundingClientRect().height;
  const growthHeight = Math.min(
    heightFromScrollHeight(textarea.scrollHeight, style),
    maxHeight
  );

  // Insertions can only keep or increase the required height. Reading scrollHeight does not
  // mutate layout, so grow directly in one step and leave an unchanged textarea untouched.
  if (growthHeight > currentHeight + 0.5) {
    const nextHeight = `${growthHeight}px`;
    if (textarea.style.height !== nextHeight) textarea.style.height = nextHeight;
    return;
  }
  if (!allowShrink) return;

  // A live textarea's scrollHeight cannot reveal content shrinkage while its explicit height is
  // larger than the content. Measure a fixed-position mirror instead of temporarily collapsing
  // the focused textarea through height:auto, which makes WebKit re-anchor the content scroller.
  const measuredHeight = Math.min(
    measureTextareaContentHeight(textarea, style),
    maxHeight
  );
  const nextHeight = `${measuredHeight}px`;
  if (
    Math.abs(measuredHeight - currentHeight) > 0.5 &&
    textarea.style.height !== nextHeight
  ) {
    textarea.style.height = nextHeight;
  }
}

export function inputMayShrinkTextarea(event: Event): boolean {
  if (!(event instanceof InputEvent)) return true;
  return (
    event.inputType.startsWith("delete") ||
    event.inputType === "historyUndo" ||
    event.inputType === "historyRedo" ||
    event.inputType === "insertReplacementText" ||
    event.inputType === "insertFromPaste" ||
    event.inputType === "insertFromDrop"
  );
}

export function settleTextareaHeightAfterBlur(
  textarea: HTMLTextAreaElement | null,
  expanded: boolean,
) {
  // Blur does not change content, so an expanded editor must keep its measured height.
  // Resetting through `height: auto` can trigger WebKit scroll anchoring between
  // pointerdown and pointerup, moving an option out from under the pointer.
  if (textarea && !expanded) textarea.style.height = "";
}
