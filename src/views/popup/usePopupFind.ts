// Popup in-page find (spec docs/specs/popup-find.md): ⌘/Ctrl+F bar, highlight, next/prev.
import {
  computed,
  nextTick,
  onBeforeUnmount,
  ref,
  watch,
  type Ref,
} from "vue";
import {
  applyFindMarks,
  clearFindMarks,
  findAllRanges,
  findHighlightableRanges,
  setCurrentFindMark,
  type TextRange,
} from "../../lib/findInDom";
import { isMac } from "../../lib/platform";
import type {
  ConfirmRequest,
  FileAttachment,
  OptionItem,
  Question,
} from "../../lib/types";
import { optionDisplayText } from "./optionDisplay";

export interface FindSegment {
  /** Stable id for DOM roots: data-find-seg */
  id: string;
  text: string;
  /** Sequential/vertical question index to reveal, if any. */
  qIndex: number | null;
}

export interface FindMatch {
  segmentId: string;
  /** 0-based index among matches in this segment. */
  occurrence: number;
  start: number;
  end: number;
  qIndex: number | null;
}

const MAX_PREFILL = 200;

export function usePopupFind(deps: {
  contentRef: Ref<HTMLElement | null>;
  isConfirm: Ref<boolean>;
  confirmRequest: Ref<ConfirmRequest | null>;
  messageText: Ref<string>;
  viewSource: Ref<boolean>;
  attachments: Ref<FileAttachment[]>;
  questions: Ref<Question[]>;
  whatsNext: Ref<boolean>;
  todoPrefix: Ref<string>;
  /** Confirm choice rows as currently displayed (label + description). */
  confirmChoiceTexts: Ref<string[]>;
  confirmTitle: Ref<string>;
  confirmSummary: Ref<string>;
  confirmToolName: Ref<string>;
  confirmBodyText: Ref<string>;
  currentQ: Ref<number>;
  verticalMode: Ref<boolean>;
  /** Reveal a question without focusing the answer composer (find navigation). */
  revealQuestion: (index: number) => void | Promise<void>;
}) {
  const findActive = ref(false);
  const findQuery = ref("");
  const findCaseSensitive = ref(false);
  /** 0-based into matches; -1 when none. */
  const findCurrent = ref(-1);
  const findInputEl = ref<HTMLInputElement | null>(null);
  const matches = ref<FindMatch[]>([]);
  let markedRoot: HTMLElement | null = null;
  let restoreFocusEl: HTMLElement | null = null;
  let applyToken = 0;

  const findTotal = computed(() => matches.value.length);
  const findCountLabel = computed(() => {
    const n = findTotal.value;
    if (!findQuery.value) return "";
    if (n === 0) return "0/0";
    return `${findCurrent.value + 1}/${n}`;
  });
  const findNoMatch = computed(
    () =>
      findActive.value && findQuery.value.length > 0 && findTotal.value === 0,
  );

  function buildSegments(): FindSegment[] {
    const segs: FindSegment[] = [];
    if (deps.isConfirm.value) {
      const cr = deps.confirmRequest.value;
      if (!cr) return segs;
      if (deps.confirmTitle.value.trim()) {
        segs.push({
          id: "confirm-title",
          text: deps.confirmTitle.value,
          qIndex: null,
        });
      }
      if (deps.confirmSummary.value.trim()) {
        segs.push({
          id: "confirm-summary",
          text: deps.confirmSummary.value,
          qIndex: null,
        });
      }
      if (deps.confirmToolName.value.trim()) {
        segs.push({
          id: "confirm-tool",
          text: deps.confirmToolName.value,
          qIndex: null,
        });
      }
      if (deps.confirmBodyText.value.trim()) {
        segs.push({
          id: "confirm-body",
          text: deps.confirmBodyText.value,
          qIndex: null,
        });
      }
      deps.confirmChoiceTexts.value.forEach((text, i) => {
        if (text.trim()) {
          segs.push({ id: `confirm-choice-${i}`, text, qIndex: null });
        }
      });
      return segs;
    }

    const msg = deps.messageText.value;
    if (msg) segs.push({ id: "message", text: msg, qIndex: null });

    deps.attachments.value.forEach((a, i) => {
      if (a.name) segs.push({ id: `att-${i}`, text: a.name, qIndex: null });
    });

    const wn = deps.whatsNext.value;
    const prefix = deps.todoPrefix.value;
    deps.questions.value.forEach((q, qi) => {
      if (q.message) {
        segs.push({ id: `q-${qi}-msg`, text: q.message, qIndex: qi });
      }
      q.predefinedOptions.forEach((opt: OptionItem, oi: number) => {
        const text = optionDisplayText(opt, wn, prefix);
        if (text) {
          segs.push({ id: `q-${qi}-opt-${oi}`, text, qIndex: qi });
        }
      });
    });
    return segs;
  }

  function segmentRanges(seg: FindSegment, query: string): TextRange[] {
    const root = deps.contentRef.value;
    const element = root?.querySelector(
      `[data-find-seg="${CSS.escape(seg.id)}"]`,
    ) as HTMLElement | null;
    return element
      ? findHighlightableRanges(element, query, findCaseSensitive.value)
      : findAllRanges(seg.text, query, findCaseSensitive.value);
  }

  function rebuildMatches(): void {
    const q = findQuery.value;
    const segs = buildSegments();
    const next: FindMatch[] = [];
    if (q) {
      for (const seg of segs) {
        const ranges = segmentRanges(seg, q);
        ranges.forEach((r, occurrence) => {
          next.push({
            segmentId: seg.id,
            occurrence,
            start: r.start,
            end: r.end,
            qIndex: seg.qIndex,
          });
        });
      }
    }
    matches.value = next;
    if (next.length === 0) {
      findCurrent.value = -1;
    } else if (findCurrent.value < 0 || findCurrent.value >= next.length) {
      findCurrent.value = 0;
    }
  }

  async function ensureQuestionVisible(qIndex: number | null): Promise<void> {
    if (qIndex === null) return;
    if (deps.currentQ.value === qIndex && !deps.verticalMode.value) {
      // Sequential already on target — still may need scroll only.
      return;
    }
    if (deps.currentQ.value === qIndex && deps.verticalMode.value) {
      // Vertical: still ask reveal to scroll the card into view.
      await deps.revealQuestion(qIndex);
      return;
    }
    await deps.revealQuestion(qIndex);
  }

  function scrollMarkIntoView(el: HTMLElement): void {
    const content = deps.contentRef.value;
    if (!content) {
      el.scrollIntoView({ block: "center", inline: "nearest" });
      return;
    }
    const cRect = content.getBoundingClientRect();
    const mRect = el.getBoundingClientRect();
    const margin = 48;
    if (
      mRect.top >= cRect.top + margin &&
      mRect.bottom <= cRect.bottom - margin
    ) {
      return;
    }
    const delta =
      mRect.top - cRect.top - (cRect.height / 2 - mRect.height / 2);
    content.scrollTop += delta;
  }

  async function applyHighlights(): Promise<void> {
    const token = ++applyToken;
    const root = deps.contentRef.value;
    if (markedRoot && markedRoot !== root) {
      clearFindMarks(markedRoot);
    }
    markedRoot = root;

    if (!findActive.value || !root || !findQuery.value) {
      if (root) clearFindMarks(root);
      return;
    }

    const anchor = matches.value[findCurrent.value] ?? null;
    if (anchor) {
      await ensureQuestionVisible(anchor.qIndex);
      if (token !== applyToken) return;
      // After sequential switch, re-read DOM-backed segment texts.
      rebuildMatches();
      if (anchor) {
        const idx = matches.value.findIndex(
          (m) =>
            m.segmentId === anchor.segmentId &&
            m.occurrence === anchor.occurrence,
        );
        if (idx >= 0) findCurrent.value = idx;
        else if (matches.value.length > 0) findCurrent.value = 0;
        else findCurrent.value = -1;
      }
    }

    await nextTick();
    if (token !== applyToken) return;

    const query = findQuery.value;
    const caseSensitive = findCaseSensitive.value;
    clearFindMarks(root);

    const segs = buildSegments();
    const allMarks: HTMLElement[] = [];
    for (const seg of segs) {
      const el = root.querySelector(
        `[data-find-seg="${CSS.escape(seg.id)}"]`,
      ) as HTMLElement | null;
      if (!el) continue;
      allMarks.push(...applyFindMarks(el, query, caseSensitive));
    }

    const mi = findCurrent.value;
    if (mi >= 0 && mi < matches.value.length) {
      const m = matches.value[mi]!;
      const segEl = root.querySelector(
        `[data-find-seg="${CSS.escape(m.segmentId)}"]`,
      ) as HTMLElement | null;
      let currentEl: HTMLElement | null = null;
      if (segEl) {
        const segMarks = Array.from(
          segEl.querySelectorAll<HTMLElement>("[data-popup-find]"),
        );
        currentEl = segMarks[m.occurrence] ?? null;
      }
      const globalIdx = currentEl ? allMarks.indexOf(currentEl) : -1;
      setCurrentFindMark(allMarks, globalIdx);
      if (currentEl) scrollMarkIntoView(currentEl);
    } else {
      setCurrentFindMark(allMarks, -1);
    }
  }

  function openFind(prefillFromSelection = true): void {
    const active = document.activeElement;
    if (active instanceof HTMLElement && !active.closest(".popup-find-bar")) {
      restoreFocusEl = active;
    }

    let prefill = "";
    if (prefillFromSelection && !findActive.value) {
      const sel = window.getSelection();
      const t = sel?.toString().trim() ?? "";
      if (t) prefill = t.slice(0, MAX_PREFILL);
    }

    // Already open: just re-focus (no re-enter animation).
    if (findActive.value) {
      findInputEl.value?.focus({ preventScroll: true });
      findInputEl.value?.select();
      return;
    }

    findActive.value = true;
    if (prefill) {
      findQuery.value = prefill;
      rebuildMatches();
      findCurrent.value = matches.value.length > 0 ? 0 : -1;
    } else if (!findQuery.value) {
      matches.value = [];
      findCurrent.value = -1;
    }

    // Focus is applied after the slide-in enter transition (FindBar @after-enter).
    void nextTick(async () => {
      await applyHighlights();
    });
  }

  function closeFind(): void {
    findActive.value = false;
    findQuery.value = "";
    findCurrent.value = -1;
    matches.value = [];
    applyToken++;
    if (markedRoot) {
      clearFindMarks(markedRoot);
      markedRoot = null;
    }
    const restore = restoreFocusEl;
    restoreFocusEl = null;
    void nextTick(() => {
      if (restore && document.contains(restore)) {
        try {
          restore.focus();
        } catch {
          /* ignore */
        }
      }
    });
  }

  async function goFind(delta: number): Promise<void> {
    if (!findActive.value) return;
    const n = matches.value.length;
    if (n === 0) return;
    const cur = findCurrent.value < 0 ? 0 : findCurrent.value;
    findCurrent.value = (cur + delta + n * 10) % n;
    await applyHighlights();
  }

  function onFindQueryInput(value: string): void {
    findQuery.value = value;
    rebuildMatches();
    findCurrent.value = matches.value.length > 0 ? 0 : -1;
    void applyHighlights();
  }

  function toggleFindCase(): void {
    findCaseSensitive.value = !findCaseSensitive.value;
    const prev = matches.value[findCurrent.value];
    rebuildMatches();
    if (prev) {
      const idx = matches.value.findIndex(
        (m) =>
          m.segmentId === prev.segmentId &&
          m.start === prev.start &&
          m.end === prev.end,
      );
      findCurrent.value =
        idx >= 0 ? idx : matches.value.length > 0 ? 0 : -1;
    } else {
      findCurrent.value = matches.value.length > 0 ? 0 : -1;
    }
    void applyHighlights();
  }

  /** Returns true if the event was handled. */
  function handleFindKeydown(e: KeyboardEvent): boolean {
    const mod = isMac ? e.metaKey : e.ctrlKey;
    const key = e.key.length === 1 ? e.key.toLowerCase() : e.key;

    // ⌘/Ctrl+F — open or refocus (no alt/shift).
    if (mod && !e.altKey && !e.shiftKey && key === "f") {
      e.preventDefault();
      if (findActive.value) {
        findInputEl.value?.focus();
        findInputEl.value?.select();
      } else {
        openFind(true);
      }
      return true;
    }

    if (!findActive.value) return false;

    // ⌘/Ctrl+G next, ⌘/Ctrl+Shift+G prev
    if (mod && !e.altKey && (key === "g" || e.key === "g" || e.key === "G")) {
      e.preventDefault();
      void goFind(e.shiftKey ? -1 : 1);
      return true;
    }

    if (e.key === "Escape") {
      e.preventDefault();
      closeFind();
      return true;
    }

    const inFindInput =
      e.target instanceof HTMLElement &&
      e.target.closest(".popup-find-bar") !== null;

    if (inFindInput && e.key === "Enter") {
      e.preventDefault();
      void goFind(e.shiftKey ? -1 : 1);
      return true;
    }

    return false;
  }

  watch(
    () =>
      [
        deps.messageText.value,
        deps.viewSource.value,
        deps.currentQ.value,
        deps.verticalMode.value,
        deps.isConfirm.value,
        deps.confirmBodyText.value,
        deps.confirmChoiceTexts.value.join("\0"),
        deps.questions.value.length,
      ] as const,
    () => {
      if (!findActive.value || !findQuery.value) return;
      rebuildMatches();
      void applyHighlights();
    },
  );

  onBeforeUnmount(() => {
    if (markedRoot) clearFindMarks(markedRoot);
  });

  return {
    findActive,
    findQuery,
    findCaseSensitive,
    findCurrent,
    findTotal,
    findCountLabel,
    findNoMatch,
    findInputEl,
    openFind,
    closeFind,
    goFind,
    onFindQueryInput,
    toggleFindCase,
    handleFindKeydown,
    /** Re-run after sequential transition settles. */
    refreshFind: () => {
      if (!findActive.value) return;
      rebuildMatches();
      void applyHighlights();
    },
  };
}
