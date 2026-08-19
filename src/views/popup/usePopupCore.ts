// 弹窗核心域：请求/确认状态、按题作答、纵向/顺序导航、键盘快捷键、初始化与生命周期。
// 语音 / 附件 / 自更新三个子域拆在 useSpeech / useAttachments / useUpdateState，由此处接线。
// 各 UI 区块子组件经 providePopupContext 注入本上下文（见 context.ts）。
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { useI18n } from "vue-i18n";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  popupInit,
  enrichPermissionDiff,
  submitPopup,
  submitConfirmAction,
  confirmPopupReady,
  cancelPopup,
  openSettings,
  openAgentConsole,
  openHistory,
  openTodos,
  openPath,
  readImageDataUrl,
  focusAgentTerminal,
  popupAgentTerminal,
  popupAgentResolved,
  popupShowWindow,
  popupImTipVisible,
  popupImTipDismiss,
  todosList,
  todosRemove,
} from "../../lib/ipc";
import { isFocusableTerminal } from "../../lib/terminals";
import {
  isWindows,
  primaryModifierPressed,
  primaryShortcutLabel,
} from "../../lib/platform";
import { matchShortcut } from "../../lib/shortcut";
import { applyLanguage } from "../../i18n";
import { renderMarkdown, handleCodeCopyClick } from "../../lib/markdown";
import { applyTheme, fileToDataUrl } from "../../lib/theme";
import { mark as perfMarkFe, enable as perfEnableFe } from "../../lib/perf";
import type {
  AskRequest,
  ConfirmChoice,
  ConfirmRequest,
  FileAttachment,
  ImageAttachment,
  PopupInit,
  PermissionDiffModel,
  PermissionEditIntent,
  Question,
  QuestionAnswer,
  ThemeMode,
  TodoEntry,
} from "../../lib/types";
import { useSpeech } from "./useSpeech";
import { useAttachments } from "./useAttachments";
import { useUpdateState } from "./useUpdateState";
import { usePopupFind } from "./usePopupFind";
import {
  canComposerDock,
  composerHomeVisibleRatio,
  DEFAULT_DOCKED_TEXTAREA_MAX_HEIGHT,
  isComposerHomeFullyVisible,
  projectedDockedComposerHomeHeight,
  resolveActionQuestionIndex,
  resolveComposerDocked,
  shouldApplyScrollSpy,
  shouldDeactivateOffscreenComposer,
  shouldRevealQuestionBeforeCmdEnter,
  type ComposerDockGeometry,
} from "./composerDock";
import {
  refreshWhatsNextTodos,
  selectedWhatsNextTodo,
} from "./whatsNextTodos";
import {
  inputMayShrinkTextarea,
  resizeTextareaToContent,
  settleTextareaHeightAfterBlur,
} from "./textareaAutosize";

export function usePopupCore() {
  const { t } = useI18n();

  // Localized labels for the markdown code-block copy button. Referencing t()
  // inside a computed keeps the rendered markdown reactive to language changes.
  const codeCopyLabels = computed(() => ({
    copyLabel: t("common.copyCode"),
    copiedLabel: t("common.copied"),
  }));

  const request = ref<AskRequest | null>(null);
  const confirmRequest = ref<ConfirmRequest | null>(null);
  const isConfirm = computed(() => confirmRequest.value !== null);
  const confirmChoiceIndex = ref<number | null>(null);
  const confirmComment = ref("");
  const permissionEdit = ref<PermissionEditIntent | null>(null);
  const permissionDiff = ref<PermissionDiffModel | null>(null);
  const permissionDiffLoading = ref(false);
  const showConfirmCloseWarning = ref(false);
  const loadError = ref<string | null>(null);

  // 视图态：false=Markdown 预览（默认），true=源码（原始文本）。作用于整篇（message + 所有问题）。
  const viewSource = ref(false);
  // 复制 message 反馈（短暂显示对勾）。
  const copiedMessage = ref(false);
  let copiedTimer: number | undefined;

  async function copyMessage() {
    try {
      await navigator.clipboard.writeText(messageText.value);
    } catch {
      /* 剪贴板不可用：静默忽略 */
    }
    copiedMessage.value = true;
    if (copiedTimer) window.clearTimeout(copiedTimer);
    copiedTimer = window.setTimeout(() => (copiedMessage.value = false), 1500);
  }

  // 当前展示的问题索引（0 始）。
  const current = ref(0);

  // 按题保存的作答状态（长度与问题数一致）。
  const chosenByQ = ref<string[][]>([]);
  const inputByQ = ref<string[]>([]);
  const imagesByQ = ref<ImageAttachment[][]>([]);
  const replyFilesByQ = ref<{ path: string; name: string }[][]>([]);
  // 每题是否已被「查看过」。
  const visited = ref<boolean[]>([]);

  const submitting = ref(false);
  /** Submit shortcut: cmdEnter (default) or enter. From popup_init / settings-updated. */
  const popupSubmitKey = ref<"cmdEnter" | "enter">("cmdEnter");
  const submitWithBareEnter = computed(() => popupSubmitKey.value === "enter");
  // Textarea refs are registered by question index.
  const inputRefs = ref<(HTMLTextAreaElement | null)[]>([]);
  function setInputRef(el: HTMLTextAreaElement | null, i: number) {
    inputRefs.value[i] = el;
  }
  const fileRef = ref<HTMLInputElement | null>(null);
  // 多问题纵向列表：滚动容器（IntersectionObserver root）+ 每题卡片 + 每题底部哨兵 + 每题缩略图容器。
  const contentRef = ref<HTMLElement | null>(null);
  const cardRefs = ref<(HTMLElement | null)[]>([]);
  function setCardRef(el: HTMLElement | null, i: number) {
    cardRefs.value[i] = el;
  }
  const sentinelRefs = ref<(HTMLElement | null)[]>([]);
  function setSentinelRef(el: HTMLElement | null, i: number) {
    sentinelRefs.value[i] = el;
  }
  const thumbsRefs = ref<(HTMLElement | null)[]>([]);
  function setThumbsRef(el: HTMLElement | null, i: number) {
    thumbsRefs.value[i] = el;
  }
  // The last explicitly focused answer editor owns bottom docking. It intentionally outlives
  // textarea focus so users can select or copy prompt text without losing the editor.
  const composerOwnerQ = ref<number | null>(null);
  const dockedComposerQ = ref<number | null>(null);
  const ownerSeenInline = ref(false);
  const ownerManuallyActivated = ref(false);
  const ownerScrolledUpAfterActivation = ref(false);
  const composerAnchorRefs = ref<(HTMLElement | null)[]>([]);
  const composerHomeRefs = ref<(HTMLElement | null)[]>([]);
  const composerDockRef = ref<HTMLElement | null>(null);
  const composerInlineHeights: number[] = [];
  const composerHomeHeights: number[] = [];
  const composerSelections: { start: number; end: number }[] = [];
  let composerResizeObserver: ResizeObserver | null = null;
  let composingQ: number | null = null;
  let pendingDockTarget: number | null | undefined;
  let returnFocusQ: number | null = null;
  let programmaticFocusActivation: {
    qIndex: number;
    manuallyActivated: boolean;
  } | null = null;
  let nextSequentialFocusIsManual = false;
  /** Find-driven sequential navigation: skip auto-focusing the answer composer. */
  let suppressNextSeqFocus = false;
  let pendingFindRevealResolve: (() => void) | null = null;
  let lastContentScrollTop = 0;
  let upwardScrollIntentUntil = 0;
  let contentScrollIntentUntil = 0;

  function ensureComposerResizeObserver(): ResizeObserver | null {
    if (composerResizeObserver || typeof ResizeObserver === "undefined") {
      return composerResizeObserver;
    }
    composerResizeObserver = new ResizeObserver((entries) => {
      for (const entry of entries) {
        const el = entry.target as HTMLElement;
        const index = Number(el.dataset.composerIndex);
        if (Number.isNaN(index) || dockedComposerQ.value === index) continue;
        const height = entry.borderBoxSize?.[0]?.blockSize ?? entry.contentRect.height;
        if (el.dataset.composerPart === "anchor") composerInlineHeights[index] = height;
        else if (el.dataset.composerPart === "home") composerHomeHeights[index] = height;
      }
      scheduleScrollWork();
    });
    return composerResizeObserver;
  }

  function replaceObservedComposerRef(
    refs: (HTMLElement | null)[],
    el: HTMLElement | null,
    i: number,
    part: "anchor" | "home"
  ) {
    const old = refs[i];
    if (old && old !== el) composerResizeObserver?.unobserve(old);
    refs[i] = el;
    if (!el) return;
    el.dataset.composerIndex = String(i);
    el.dataset.composerPart = part;
    ensureComposerResizeObserver()?.observe(el);
    const height = el.getBoundingClientRect().height;
    if (dockedComposerQ.value !== i) {
      if (part === "anchor") composerInlineHeights[i] = height;
      else composerHomeHeights[i] = height;
    }
  }

  function setComposerAnchorRef(el: HTMLElement | null, i: number) {
    replaceObservedComposerRef(composerAnchorRefs.value, el, i, "anchor");
  }

  function setComposerHomeRef(el: HTMLElement | null, i: number) {
    replaceObservedComposerRef(composerHomeRefs.value, el, i, "home");
  }

  function setComposerDockRef(el: HTMLElement | null) {
    composerDockRef.value = el;
  }

  function composerAnchorStyle(i: number): Record<string, string> | undefined {
    if (dockedComposerQ.value !== i) return undefined;
    const height = composerInlineHeights[i] ?? 0;
    return height > 0 ? { height: `${height}px` } : undefined;
  }

  function rememberComposerSelection(i: number) {
    const el = inputRefs.value[i];
    if (!el) return;
    composerSelections[i] = {
      start: el.selectionStart ?? el.value.length,
      end: el.selectionEnd ?? el.selectionStart ?? el.value.length,
    };
  }

  function setDockedComposer(next: number | null) {
    if (dockedComposerQ.value === next) return;
    const movingQ = next ?? dockedComposerQ.value ?? composerOwnerQ.value;
    if (movingQ === null) {
      dockedComposerQ.value = next;
      return;
    }
    if (composingQ === movingQ) {
      pendingDockTarget = next;
      return;
    }

    const el = inputRefs.value[movingQ];
    const hadFocus = !!el && document.activeElement === el;
    const selection = el
      ? {
          start: el.selectionStart ?? el.value.length,
          end: el.selectionEnd ?? el.selectionStart ?? el.value.length,
        }
      : composerSelections[movingQ];
    if (selection) composerSelections[movingQ] = selection;
    const shouldFocus = hadFocus || returnFocusQ === movingQ;
    dockedComposerQ.value = next;
    nextTick(() => {
      const moved = inputRefs.value[movingQ];
      if (shouldFocus && moved) {
        focusComposer(movingQ, ownerManuallyActivated.value);
        if (selection) moved.setSelectionRange(selection.start, selection.end);
      }
      if (returnFocusQ === movingQ && next === null) returnFocusQ = null;
      autoGrow(movingQ);
      scheduleScrollWork();
    });
  }

  function composerGeometry(i: number): ComposerDockGeometry | null {
    const root = contentRef.value;
    const anchor = composerAnchorRefs.value[i];
    if (!root || !anchor) return null;
    const viewport = root.getBoundingClientRect();
    const anchorRect = anchor.getBoundingClientRect();
    const liveHomeHeight =
      dockedComposerQ.value === i
        ? 0
        : composerHomeRefs.value[i]?.getBoundingClientRect().height ?? 0;
    if (liveHomeHeight > 0) composerHomeHeights[i] = liveHomeHeight;
    const homeHeight = composerHomeHeights[i] ?? 0;
    if (homeHeight <= 0) return null;
    const textarea = inputRefs.value[i];
    const liveTextareaHeight =
      dockedComposerQ.value === i
        ? 0
        : textarea?.getBoundingClientRect().height ?? 0;
    const configuredDockedTextareaMaxHeight = textarea
      ? Number.parseFloat(
          getComputedStyle(textarea).getPropertyValue(
            "--composer-docked-textarea-max-height"
          )
        )
      : Number.NaN;
    const dockedTextareaMaxHeight = Number.isFinite(
      configuredDockedTextareaMaxHeight
    )
      ? configuredDockedTextareaMaxHeight
      : DEFAULT_DOCKED_TEXTAREA_MAX_HEIGHT;
    const liveDockedHomeHeight =
      dockedComposerQ.value === i
        ? composerHomeRefs.value[i]?.getBoundingClientRect().height ?? 0
        : 0;
    const dockedHomeHeight =
      liveDockedHomeHeight > 0
        ? liveDockedHomeHeight
        : projectedDockedComposerHomeHeight(
            homeHeight,
            liveTextareaHeight,
            dockedTextareaMaxHeight
          );
    if (dockedHomeHeight <= 0) return null;
    const releasedHeight =
      dockedComposerQ.value === i
        ? composerDockRef.value?.getBoundingClientRect().height ?? 0
        : 0;
    return {
      homeTop: anchorRect.top,
      homeBottom: anchorRect.top + homeHeight,
      dockedHomeHeight,
      viewportTop: viewport.top,
      viewportBottom: viewport.bottom,
      viewportBottomAfterUndock: viewport.bottom + releasedHeight,
    };
  }

  function measureComposerDock() {
    const i = composerOwnerQ.value;
    if (i === null) {
      if (dockedComposerQ.value !== null) setDockedComposer(null);
      return;
    }
    if (dockedComposerQ.value !== null && dockedComposerQ.value !== i) {
      setDockedComposer(null);
      return;
    }
    const geometry = composerGeometry(i);
    if (!geometry) return;
    const currentlyDocked = dockedComposerQ.value === i;
    if (!currentlyDocked && isComposerHomeFullyVisible(geometry)) {
      ownerSeenInline.value = true;
    }
    const shouldDock = resolveComposerDocked(
      currentlyDocked,
      currentlyDocked ||
        canComposerDock(
          focusedQ.value === i,
          ownerManuallyActivated.value,
          ownerSeenInline.value,
          ownerScrolledUpAfterActivation.value
        ),
      geometry
    );
    if (shouldDock !== currentlyDocked) setDockedComposer(shouldDock ? i : null);
  }

  function activateComposer(i: number, manuallyActivated = true) {
    if (composerOwnerQ.value !== i) {
      if (dockedComposerQ.value !== null) setDockedComposer(null);
      composerOwnerQ.value = i;
      ownerSeenInline.value = false;
      ownerManuallyActivated.value = manuallyActivated;
      ownerScrolledUpAfterActivation.value = false;
      lastContentScrollTop = contentRef.value?.scrollTop ?? 0;
      returnFocusQ = null;
    } else if (manuallyActivated) {
      ownerManuallyActivated.value = true;
      if (dockedComposerQ.value === null) {
        ownerScrolledUpAfterActivation.value = false;
        lastContentScrollTop = contentRef.value?.scrollTop ?? 0;
      }
    }
    nextTick(() => {
      const geometry = composerGeometry(i);
      if (geometry && isComposerHomeFullyVisible(geometry)) {
        ownerSeenInline.value = true;
      }
      scheduleScrollWork();
    });
  }

  function focusComposer(i: number, manuallyActivated: boolean) {
    const el = inputRefs.value[i];
    if (!el) return;
    programmaticFocusActivation = { qIndex: i, manuallyActivated };
    el.focus({ preventScroll: true });
    if (programmaticFocusActivation?.qIndex === i) {
      programmaticFocusActivation = null;
      activateComposer(i, manuallyActivated);
    }
  }

  function focusComposerIfInitiallyVisible(i: number) {
    const geometry = composerGeometry(i);
    if (!geometry || composerHomeVisibleRatio(geometry) < 0.5) return;
    focusComposer(i, false);
  }

  function clearComposerOwner() {
    if (dockedComposerQ.value !== null) setDockedComposer(null);
    composerOwnerQ.value = null;
    ownerSeenInline.value = false;
    ownerManuallyActivated.value = false;
    ownerScrolledUpAfterActivation.value = false;
    returnFocusQ = null;
  }

  function endOtherComposer(i: number) {
    const focused = focusedQ.value;
    if (focused !== null && focused !== i) {
      inputRefs.value[focused]?.blur();
    }
    if (composerOwnerQ.value !== null && composerOwnerQ.value !== i) {
      clearComposerOwner();
    }
  }

  function onComposerCompositionStart(i: number) {
    composingQ = i;
  }

  function onComposerCompositionEnd(i: number) {
    if (composingQ === i) composingQ = null;
    if (pendingDockTarget !== undefined) {
      pendingDockTarget = undefined;
      scheduleScrollWork();
    }
  }

  function resetComposerDock() {
    for (const el of composerAnchorRefs.value) if (el) composerResizeObserver?.unobserve(el);
    for (const el of composerHomeRefs.value) if (el) composerResizeObserver?.unobserve(el);
    composerOwnerQ.value = null;
    dockedComposerQ.value = null;
    ownerSeenInline.value = false;
    ownerManuallyActivated.value = false;
    ownerScrolledUpAfterActivation.value = false;
    composerAnchorRefs.value = [];
    composerHomeRefs.value = [];
    composerInlineHeights.length = 0;
    composerHomeHeights.length = 0;
    composerSelections.length = 0;
    composingQ = null;
    pendingDockTarget = undefined;
    returnFocusQ = null;
    programmaticFocusActivation = null;
    nextSequentialFocusIsManual = false;
    lastContentScrollTop = contentRef.value?.scrollTop ?? 0;
    upwardScrollIntentUntil = 0;
    contentScrollIntentUntil = 0;
  }
  // 当前聚焦的问题索引（null = 无）；驱动折叠输入框展开。
  const focusedQ = ref<number | null>(null);
  // 待归属图片的目标题（「添加图片」按钮点选时设置）。
  let pendingPickQ = 0;
  // 弹窗刚上屏（原生「出现」动画期）短时吞掉 ⌘W：用户常按 ⌘W 关别的窗口，弹窗恰好此刻弹出会被误关。
  const APPEAR_GUARD_MS = 500;
  let appearGuardUntil = 0;
  // 键盘/按钮 setActive 后短暂锁定，避免随即的滚动事件把 active 改回去。
  let activeLockUntil = 0;
  // 导航滚动锁时长：需覆盖「聚焦展开(双 rAF) + smooth 滚动动画」的整个过程，否则动画尾段的滚动事件会让
  // scroll-spy 把 current 抢到别题（露出题顶=对齐顶部定位时，阅读线并不落在目标题，故必须锁到动画结束）。
  const NAV_LOCK_MS = 700;
  let io: IntersectionObserver | null = null;
  const scrolled = ref(false);
  // 内容是否已滚到最顶（scrollTop<=0）：用于纵向模式判断「首题的『上一个』能否再往上露出 message」。
  const atTop = ref(true);
  // 按住 ⌘/Ctrl 时高亮右侧快捷键 Badge（提示「此刻按数字即可选项」）。
  const cmdHeld = ref(false);
  // 取消二次确认（已有部分回答时）。
  const showCancelConfirm = ref(false);

  function onScroll(e: Event) {
    const st = (e.target as HTMLElement).scrollTop;
    if (
      composerOwnerQ.value !== null &&
      Date.now() <= upwardScrollIntentUntil &&
      st < lastContentScrollTop - 0.5
    ) {
      ownerScrolledUpAfterActivation.value = true;
    }
    lastContentScrollTop = st;
    scrolled.value = st > 0;
    atTop.value = st <= 0;
    scheduleScrollWork(true);
  }

  function onContentWheel(e: WheelEvent) {
    if (e.deltaY !== 0) contentScrollIntentUntil = Date.now() + 500;
    if (e.deltaY < 0) upwardScrollIntentUntil = Date.now() + 500;
  }

  // 滚动定位「当前题」：用「比例阅读线」(proportional scroll-spy)——判定线在视口内的纵向位置
  // 随滚动进度 p=scrollTop/maxScroll 从顶部线性扫到底部（p=0→视口顶、p=1→视口底）。active =
  // 该线当前落在的题（即最后一个 top ≤ 线的题）。如此滚动进度被均匀分配给各题：滚到最顶=第一题、
  // 滚到底=末题、中间进度=中间题，**每题都有一段可达区间**（修复「内容仅略超视口时，一滑就从首题
  // 跳到末题、中间题选不中」）；且因用真实卡片边界，超长题在其铺满视口期间持续保持 active（高度自适应）。
  // Keep scroll-spy from overriding keyboard/button navigation during NAV_LOCK_MS.
  function readingLineY(root: HTMLElement): number {
    const r = root.getBoundingClientRect();
    const max = root.scrollHeight - root.clientHeight;
    const p = max > 0 ? Math.min(1, Math.max(0, root.scrollTop / max)) : 0;
    return r.top + p * root.clientHeight;
  }
  function activeForScroll(root: HTMLElement): number {
    const line = readingLineY(root);
    let next = 0;
    for (let i = 0; i < cardRefs.value.length; i++) {
      const el = cardRefs.value[i];
      if (!el) continue;
      if (el.getBoundingClientRect().top <= line) next = i;
    }
    return next;
  }
  let scrollRaf = 0;
  let scrollSpyPending = false;
  function scheduleScrollWork(fromScrollEvent = false) {
    // Geometry-only callers (keyboard activation, ResizeObserver, Teleport) must not hand the
    // current-question pointer back to scroll-spy. Preserve a real scroll intent if calls coalesce.
    if (fromScrollEvent) scrollSpyPending = true;
    if (scrollRaf) return;
    scrollRaf = requestAnimationFrame(() => {
      scrollRaf = 0;
      const hadScrollEvent = scrollSpyPending;
      const now = Date.now();
      const applyScrollSpy = shouldApplyScrollSpy(
        hadScrollEvent,
        verticalMode.value,
        now,
        activeLockUntil,
      );
      scrollSpyPending = false;
      if (applyScrollSpy) {
        const root = contentRef.value;
        if (root) {
          const next = activeForScroll(root);
          if (next !== current.value) current.value = next;
        }
      }
      measureComposerDock();
      // User wheel/trackpad input may intentionally override an in-flight navigation lock. Outside
      // the lock, any real content scroll is eligible (including scrollbar-driven scrolling).
      if (
        hadScrollEvent &&
        (applyScrollSpy || now <= contentScrollIntentUntil)
      ) {
        deactivateOffscreenComposer();
      }
    });
  }

  // 建立/重建底部哨兵观察：哨兵进视口 → 该题「已看到」（兼容超长题）。
  function setupQuestionObserver() {
    io?.disconnect();
    io = null;
    if (!verticalMode.value) return;
    const root = contentRef.value;
    if (!root) return;
    io = new IntersectionObserver(
      (entries) => {
        for (const en of entries) {
          if (!en.isIntersecting) continue;
          const idx = Number((en.target as HTMLElement).dataset.qSentinel);
          if (!Number.isNaN(idx)) markVisited(idx);
        }
      },
      { root, threshold: 0 }
    );
    for (const el of sentinelRefs.value) if (el) io.observe(el);
  }

  const pinned = ref(false);
  const sourceName = ref("the Loop");
  // 来源 workspace：名称用于标题区展示，完整路径用于 hover 提示。空则隐藏该元素。
  const projectName = ref("");
  const projectPath = ref("");
  // 来源 agent：家族标识 + pid + 所在终端类型（决定 badge 是否可点击激活 tab）。
  const agentKind = ref("");
  const agentPid = ref<number | null>(null);
  const agentLaunchId = ref<string | null>(null);
  const agentConsoleSessionId = ref("");
  const agentTerminal = ref<string | null>(null);
  // agent badge 文案：本地化已知家族名；未知家族回退原始标识。
  const agentLabel = computed(() => {
    const k = agentKind.value;
    if (!k) return "";
    const label = t(`agents.kind.${k}`);
    return label === `agents.kind.${k}` ? k : label;
  });
  // macOS uses a pid/TTY; Windows uses a daemon-registered launch UUID.
  const agentFocusable = computed(
    () =>
      (!!agentPid.value || !!agentLaunchId.value) &&
      isFocusableTerminal(agentTerminal.value)
  );
  const agentConsoleAvailable = computed(
    () => agentConsoleSessionId.value.length > 0
  );

  // 点击 agent badge：聚焦该 agent 所在终端的 tab（失败静默，仅日志）。
  async function onFocusAgentTerminal() {
    if (!agentFocusable.value) return;
    try {
      await focusAgentTerminal(agentPid.value, agentLaunchId.value);
    } catch (err) {
      console.warn("focus agent terminal failed", err);
    }
  }

  // daemon 已严格匹配会话时才会显示；后端从自身状态取目标 session，前端不参与寻址。
  function openAgentConsoleWindow() {
    openAgentConsole().catch((err) => {
      console.warn("open agent console failed", err);
    });
  }

  // 点击 workspace badge：在文件管理器打开该目录。
  async function onOpenWorkspace() {
    if (!projectPath.value) return;
    try {
      await openPath(projectPath.value);
    } catch (err) {
      console.warn("open workspace failed", err);
    }
  }

  const questions = computed<Question[]>(() => request.value?.questions ?? []);
  const total = computed(() => questions.value.length);
  const isMulti = computed(() => total.value > 1);
  // 实验开关：多问题是否纵向同时显示（来自 popup_init）。verticalMode = 开关开 且 多问题。
  // 关 / 单问题 → 旧版「一次一题 + 上/下一步」（sequential）。
  const verticalEnabled = ref(false);
  const verticalMode = computed(() => verticalEnabled.value && isMulti.value);
  // 拖拽悬停中的目标题（纵向布局才有意义）：驱动卡片高亮，松手前即可看出落点。
  const dropTargetQ = ref<number | null>(null);
  // Passive scrolling only changes `current` (the viewport card). While an editor retains DOM
  // focus, every user action stays owned by that editor until an explicit cross-question action.
  const actionQuestionIndex = computed(() =>
    verticalMode.value
      ? resolveActionQuestionIndex(current.value, focusedQ.value)
      : current.value
  );
  const actionQuestion = computed<Question | null>(
    () => questions.value[actionQuestionIndex.value] ?? null
  );
  const inputRef = computed<HTMLTextAreaElement | null>(
    () => inputRefs.value[actionQuestionIndex.value] ?? null
  );
  // 严格选择：隐藏补充输入 / 附件区，且必须选中才能提交（D11）。
  const selectOnly = computed(() => request.value?.selectOnly ?? false);
  // 单选：选项渲染为 radio，每题恰好一个（D11）。
  const single = computed(() => request.value?.single ?? false);
  // whats-next 提问（spec todo-whats-next D2/D7）：待办已是问题选项本体，折叠区不重复渲染 chip。
  const whatsNext = computed(() => request.value?.whatsNext ?? false);
  let whatsNextStaticOptions: Question["predefinedOptions"] = [];
  let whatsNextBaseMessage = "";

  // ===== 待办下拉区（spec todo-whats-next D7，第 11 轮改版）=====
  // 该提问项目的待办列表；直读 todos.json（经后端命令），渲染后异步加载不阻塞首屏。
  const todos = ref<TodoEntry[]>([]);
  const todosOpen = ref(false);
  // 选中的待办条目 id（提交时文本并入回答、id 送后端出队）。
  const todoChosenIds = ref<string[]>([]);
  const todoChosenSnapshots = ref<Record<string, TodoEntry>>({});
  // 点选作答在严格选择（禁自由文本）之外都启用；选中文本恒并入**最后一题**的回答。
  const todoChipsEnabled = computed(() => !selectOnly.value);
  // 跟在最后一个问题后面、仅在有待办时显示：whats-next 弹窗不显示（待办已是选项本体）；
  // 无项目 key（未知 workspace）时无法归属，隐藏；sequential 多题模式只在最后一题的面板显示。
  const todoSectionVisible = computed(
    () =>
      !!projectPath.value &&
      !!request.value &&
      !whatsNext.value &&
      todos.value.length > 0 &&
      (verticalMode.value || current.value === total.value - 1)
  );
  const selectedTodos = computed(() =>
    todoChosenIds.value
      .map((id) => todoChosenSnapshots.value[id])
      .filter((todo): todo is TodoEntry => !!todo)
  );

  let todoLoadGeneration = 0;
  async function loadTodos() {
    if (!projectPath.value || !request.value) return;
    const generation = ++todoLoadGeneration;
    try {
      const latest = await todosList(projectPath.value);
      if (generation !== todoLoadGeneration) return;
      const liveIds = new Set(latest.map((todo) => todo.id));
      todos.value = latest;
      if (whatsNext.value) {
        const question = request.value.questions[0];
        if (!question) return;
        const refreshed = refreshWhatsNextTodos(
          question.predefinedOptions,
          whatsNextStaticOptions,
          latest,
          chosenByQ.value[0] ?? [],
          t("popup.todos.optionPrefix"),
          (count) => t("common.attachmentBadge", { n: count }),
        );
        question.predefinedOptions = refreshed.options;
        question.message = refreshed.hiddenTodos
          ? `${whatsNextBaseMessage}\n\n${t("popup.todos.more", { n: refreshed.hiddenTodos })}`
          : whatsNextBaseMessage;
        chosenByQ.value[0] = refreshed.selectedOptions;
        return;
      }
      // External deletes/completions must also clear stale local selections.
      todoChosenIds.value = todoChosenIds.value.filter((id) => liveIds.has(id));
      for (const id of Object.keys(todoChosenSnapshots.value)) {
        if (!liveIds.has(id)) delete todoChosenSnapshots.value[id];
      }
    } catch {
      /* 旧后端无此命令：待办区保持空 */
    }
  }

  function toggleTodo(id: string) {
    if (!todoChipsEnabled.value) return;
    const i = todoChosenIds.value.indexOf(id);
    if (i >= 0) {
      todoChosenIds.value.splice(i, 1);
      delete todoChosenSnapshots.value[id];
    } else {
      const todo = todos.value.find((entry) => entry.id === id);
      if (!todo) return;
      todoChosenSnapshots.value[id] = {
        ...todo,
        attachments: (todo.attachments ?? []).map((attachment) => ({ ...attachment })),
      };
      todoChosenIds.value.push(id);
    }
  }

  async function removeTodo(id: string) {
    todos.value = todos.value.filter((td) => td.id !== id);
    todoChosenIds.value = todoChosenIds.value.filter((x) => x !== id);
    delete todoChosenSnapshots.value[id];
    try {
      await todosRemove(projectPath.value, id);
    } catch {
      /* best-effort：条目已不在文件中也无妨（spec D11） */
    }
  }
  const currentQuestion = computed<Question | null>(
    () => questions.value[current.value] ?? null
  );
  // 旧版（sequential）单题代理：作用于「当前题」，供 v-else 单题面板复用旧模板写法。
  const chosen = computed(() => chosenByQ.value[current.value] ?? []);
  const userInput = computed<string>({
    get: () => inputByQ.value[current.value] ?? "",
    set: (v) => {
      if (current.value < inputByQ.value.length) inputByQ.value[current.value] = v;
    },
  });
  const images = computed(() => imagesByQ.value[current.value] ?? []);
  const replyFiles = computed(() => replyFilesByQ.value[current.value] ?? []);
  // 旧版切题左右滑动方向 + 过渡名；「全部看过」用于 sequential 模式显示发送按钮。
  const slideDir = ref<"next" | "prev">("next");
  const transitionName = computed(() =>
    slideDir.value === "next" ? "q-slide-next" : "q-slide-prev"
  );
  const allViewed = computed(
    () => visited.value.length > 0 && visited.value.every(Boolean)
  );
  // 旧版单题面板的头部 ref（切题时滚到顶）。
  const qHeaderRef = ref<HTMLElement | null>(null);
  // 共享 Message（描述 + 附件）。无 -q 时 text 为空（第一个参数已提升为问题）。
  const messageText = computed(() => request.value?.message.text ?? "");
  const showDescription = computed(
    () => messageText.value.trim() !== "" || attachments.value.length > 0
  );
  // 存在 Message（描述/附件）或多题时，显示问题头部以区隔 Message 与 Question。
  const showQuestionHeader = computed(() => showDescription.value || isMulti.value);
  // 顶栏来源头部：默认来源 "the Loop"（human-in-the-loop 固定短语）强制英文；
  // 自定义来源跟随界面语言（与后端 i18n::source_header 规则一致）。
  const DEFAULT_SOURCE_NAME = "the Loop";
  const headerTitle = computed(() => {
    const key = showQuestionHeader.value ? "popup.messageFrom" : "popup.questionFrom";
    const named = { source: sourceName.value };
    return sourceName.value === DEFAULT_SOURCE_NAME
      ? t(key, named, { locale: "en" })
      : t(key, named);
  });
  // 标题内联 agent 胶囊：探测到 agent 且未定制来源名时，把「the Loop」文字替换为 agent 胶囊本身
  // （后端未定制时已把来源名解析为 agent 名，故 sourceName 可能等于 agentLabel；MCP 下后端回退
  // "the Loop" 但前端经 AgentResolved 拿到 agentKind，故再判默认值）。
  const agentInline = computed(
    () =>
      !!agentLabel.value &&
      (sourceName.value === DEFAULT_SOURCE_NAME ||
        sourceName.value === agentLabel.value)
  );
  // 内联模式把「Message from {source}」按 {source} 占位切成前后两段，胶囊嵌入中间。
  // 文案强制英文（与默认 "the Loop" 一致）：英文 source 在句尾，故前缀「Message from」、后缀为空，
  // 渲染为「Message from [Cursor] [Project]」，规避中文「来自 [胶囊] 的消息」夹在句中的观感。
  const SOURCE_SLOT = "\u0000";
  const headerParts = computed(() => {
    const key = showQuestionHeader.value ? "popup.messageFrom" : "popup.questionFrom";
    const full = t(key, { source: SOURCE_SLOT }, { locale: "en" });
    const idx = full.indexOf(SOURCE_SLOT);
    if (idx < 0) return { prefix: full, suffix: "" };
    // 去掉占位两侧空白，由 .brand 的 flex gap 统一提供胶囊间距，避免空格叠加。
    return {
      prefix: full.slice(0, idx).trimEnd(),
      suffix: full.slice(idx + SOURCE_SLOT.length).trimStart(),
    };
  });
  // 统一渲染：非内联 → 前缀=完整标题、后缀空；内联 → 拆分后的前后缀（胶囊夹在中间）。
  const headerPrefix = computed(() =>
    agentInline.value ? headerParts.value.prefix : headerTitle.value
  );
  const headerSuffix = computed(() =>
    agentInline.value ? headerParts.value.suffix : ""
  );
  // 弹窗头部时间：提问创建时刻（epoch ms，daemon 上送；冷/单进程为弹窗构造时刻）。0/缺失=不显示。
  const createdAtMs = ref(0);
  // 每秒 tick，让相对时间随停留时长自动走字（与请求内容解耦）。
  const nowMs = ref(Date.now());
  let timeTicker: number | undefined;
  // 相对时间：满一天改绝对时间（Q2 甲）。<5s 刚刚 / <60s N 秒前 / <60min N 分钟前 / <24h N 小时前。
  const popupTimeRel = computed(() => {
    const created = createdAtMs.value;
    if (!created) return "";
    const diff = Math.max(0, Math.floor((nowMs.value - created) / 1000));
    if (diff < 5) return t("popup.time.justNow");
    if (diff < 60) return t("popup.time.secondsAgo", { n: diff });
    const min = Math.floor(diff / 60);
    if (min < 60) return t("popup.time.minutesAgo", { n: min });
    const hr = Math.floor(min / 60);
    if (hr < 24) return t("popup.time.hoursAgo", { n: hr });
    // 满一天：绝对时间（跟随系统语言/格式）。
    return new Date(created).toLocaleString();
  });
  // hover 精确绝对时间（title）。
  const popupTimeAbs = computed(() =>
    createdAtMs.value ? new Date(createdAtMs.value).toLocaleString() : ""
  );
  // 多题显示「Question i/n」；单题（仅在有 Message 时显示头部）只显示「Question」。
  const questionHeaderLabel = computed(() =>
    isMulti.value
      ? t("popup.question.indexed", { i: current.value + 1, n: total.value })
      : t("popup.question.single")
  );
  // 「已看到」：卡片底部进视口（IntersectionObserver）或曾被设为当前题（setActive）。
  // 多问题发送按钮出现条件 = 最后一题已看到；单问题恒真。
  const lastSeen = computed(
    () => !isMulti.value || (visited.value[total.value - 1] ?? false)
  );
  const hasAnyAnswer = computed(
    () =>
      questions.value.some((_, i) => isAnswered(i)) ||
      todoChosenIds.value.length > 0
  );
  // 严格选择下「必须选中才能提交」：每个有选项的问题都需至少一个勾选。
  const canSubmit = computed(() => {
    if (!selectOnly.value) return true;
    return questions.value.every(
      (_, i) => (chosenByQ.value[i]?.length ?? 0) > 0
    );
  });
  // Passive scrolling does not change whether the unified action target is the last question.
  const onLastQuestion = computed(
    () => actionQuestionIndex.value === total.value - 1
  );

  // 「上一个」是否可用：纵向模式下即使在首题，只要还没滚到最顶（上方 message 未露全）就可用（点它=露出 message）；
  // 旧版顺序模式仍是「非首题才可用」。
  const canGoPrev = computed(() =>
    verticalMode.value
      ? !(actionQuestionIndex.value === 0 && atTop.value)
      : current.value > 0
  );

  const cmdEnterFromQ = computed(() => actionQuestionIndex.value);
  // 纵向模式下 ⌘↵ 是否会「提交」：已看完全部 且 快捷键目标题之后再无未答题（含焦点在末题时恒真）。
  // 与 onCmdEnter 的分支完全一致——「谁挂 ⌘↵ = ⌘↵ 就干谁」，故 ⌘↵ 角标据此挂在提交按钮上。
  const cmdEnterWillSubmit = computed(
    () =>
      verticalMode.value &&
      lastSeen.value &&
      nextUnansweredAfter(cmdEnterFromQ.value) < 0
  );
  // 提交按钮挂发送快捷键角标 / 为主按钮：纵向按 cmdEnterWillSubmit，旧版顺序沿用 onLastQuestion。
  const submitShowsCmdEnter = computed(() =>
    verticalMode.value ? cmdEnterWillSubmit.value : onLastQuestion.value
  );
  /** Label for the submit shortcut badge (⌘↵ vs ↵). */
  const submitKeyLabel = computed(() =>
    submitWithBareEnter.value ? "↵" : primaryShortcutLabel("enter")
  );
  const submitPrimary = computed(() => submitShowsCmdEnter.value);
  // 下一个是否主按钮：末题从不主；否则在「提交尚未成为主按钮」时为主（读题引导）。
  const nextPrimary = computed(
    () => !onLastQuestion.value && !submitPrimary.value
  );

  // CMD+数字 选项快捷键上限（1-9）；超出的选项不分配快捷键。
  const OPTION_HOTKEY_MAX = 9;
  function optionHotkey(i: number): string | null {
    return i < OPTION_HOTKEY_MAX ? primaryShortcutLabel(String(i + 1)) : null;
  }

  function isAnswered(i: number): boolean {
    return (
      (chosenByQ.value[i]?.length ?? 0) > 0 ||
      (inputByQ.value[i]?.trim().length ?? 0) > 0 ||
      (imagesByQ.value[i]?.length ?? 0) > 0 ||
      (replyFilesByQ.value[i]?.length ?? 0) > 0
    );
  }

  // 折叠输入仅在纵向模式生效：默认 1 行，聚焦或已有内容时展开。单题 / 旧版顺序模式恒展开。
  function expandedQ(i: number): boolean {
    if (!verticalMode.value) return true;
    return (
      dockedComposerQ.value === i ||
      focusedQ.value === i ||
      (inputByQ.value[i]?.trim().length ?? 0) > 0
    );
  }
  // 仅「当前题」显示 ⌘1–9 角标（避免每题都冒出 ⌘1）。
  function cardOptionHotkey(qIndex: number, optIndex: number): string | null {
    if (isMulti.value && qIndex !== actionQuestionIndex.value) return null;
    return optionHotkey(optIndex);
  }

  // 提问附带的文件附件（AI→人，仅展示）：Message 级，顶部常驻，不随题切换。
  const attachments = computed<FileAttachment[]>(
    () => request.value?.message.files ?? []
  );

  // ===== 子域接线：附件 / 自更新 =====
  const attach = useAttachments({ attachments });
  const update = useUpdateState({ codeCopyLabels });

  // 托盘「待答」子菜单点击本弹窗时，边框闪烁一次（accent 蓝脉冲）。
  const flashing = ref(false);
  let flashTimer: number | undefined;
  let unlistenDrop: UnlistenFn | null = null;
  let unlistenSettings: UnlistenFn | null = null;
  let unlistenCloseReq: UnlistenFn | null = null;
  let unlistenFlash: UnlistenFn | null = null;
  let unlistenAgent: UnlistenFn | null = null;
  let unlistenTodos: UnlistenFn | null = null;
  // 方案6 预热弹窗：daemon 领用时 emit 的唤醒事件，前端据此 pull 请求并渲染。
  let unlistenShow: UnlistenFn | null = null;

  function triggerFlash() {
    // 重启动画：先关再于下一帧开，确保连续点击也能重新触发。
    flashing.value = false;
    if (flashTimer) window.clearTimeout(flashTimer);
    requestAnimationFrame(() => {
      flashing.value = true;
      // 两次脉冲约 0.6s 后复位。
      flashTimer = window.setTimeout(() => {
        flashing.value = false;
      }, 700);
    });
  }

  // 渲染后的 Markdown 里的链接：用系统默认浏览器打开，避免在弹窗 webview 内跳转。
  function onContentClick(e: MouseEvent) {
    // 代码块的拷贝按钮优先处理（命中即结束，不再走链接逻辑）。
    if (handleCodeCopyClick(e)) return;
    const anchor = (e.target as HTMLElement | null)?.closest?.("a") as
      | HTMLAnchorElement
      | null;
    if (!anchor) return;
    const href = anchor.href;
    if (!/^(https?:|mailto:)/i.test(href)) return;
    e.preventDefault();
    e.stopPropagation();
    openPath(href).catch(() => {});
  }

  async function togglePin() {
    pinned.value = !pinned.value;
    try {
      await getCurrentWindow().setAlwaysOnTop(pinned.value);
    } catch {
      pinned.value = !pinned.value;
    }
  }

  function openSettingsWindow() {
    openSettings().catch(() => {});
  }

  // ===== 首次运行引导（R6）：无 IM 渠道时页脚上方的一次性提示 =====
  const imTipVisible = ref(false);

  function imTipConfigure() {
    imTipVisible.value = false;
    popupImTipDismiss().catch(() => {});
    openSettings("channel").catch(() => {});
  }

  function imTipDismiss() {
    imTipVisible.value = false;
    popupImTipDismiss().catch(() => {});
  }

  function openHistoryWindow() {
    openHistory().catch(() => {});
  }

  function openTodosWindow() {
    openTodos(projectPath.value || null).catch(() => {});
  }

  // 切换某题的选项（带题索引，供选项点击 / CMD+数字 复用）。
  function toggle(qIndex: number, option: string) {
    const arr = chosenByQ.value[qIndex];
    if (!arr) return;
    if (
      (focusedQ.value !== null && focusedQ.value !== qIndex) ||
      (composerOwnerQ.value !== null && composerOwnerQ.value !== qIndex)
    ) {
      speech.stopListening();
    }
    endOtherComposer(qIndex);
    const i = arr.indexOf(option);
    // 单选：选中即替换为唯一项；再次点击当前选中项则清空（保留"可不选"，除非严格模式）。
    if (single.value) {
      if (i >= 0) arr.splice(0, arr.length);
      else arr.splice(0, arr.length, option);
      return;
    }
    if (i >= 0) arr.splice(i, 1);
    else arr.push(option);
  }

  // Toggle an option on the unified action target. If passive scrolling moved its card away,
  // Cmd+1–9 reveals the card after selecting while retaining editor focus.
  function toggleByIndex(i: number) {
    const qIndex = actionQuestionIndex.value;
    const opts = actionQuestion.value?.predefinedOptions;
    if (!opts || i < 0 || i >= opts.length) return;
    toggle(qIndex, opts[i].text);
    if (!verticalMode.value) return;
    setActive(qIndex, false);
    activeLockUntil = Date.now() + NAV_LOCK_MS;
    nextTick(() => {
      activeLockUntil = Date.now() + NAV_LOCK_MS;
      scrollQuestionIntoView(qIndex);
    });
  }

  // 点「添加图片」：记录目标题后唤起文件选择。
  function pickFiles(qIndex: number) {
    activateComposer(qIndex);
    pendingPickQ = qIndex;
    fileRef.value?.click();
  }

  async function addFiles(files: FileList | File[], qIndex: number) {
    if (selectOnly.value) return;
    endOtherComposer(qIndex);
    let added = 0;
    for (const file of Array.from(files)) {
      if (!file.type.startsWith("image/")) continue;
      const data = await fileToDataUrl(file);
      imagesByQ.value[qIndex]?.push({
        data,
        mediaType: file.type,
        filename: file.name,
      });
      added++;
    }
    if (added) scrollImagesIntoView(qIndex);
  }

  // 新增图片后把该题最新缩略图滚入可见区：粘贴/选择时即使内容已上滚，也能立刻确认成功。
  async function scrollImagesIntoView(qIndex: number) {
    await nextTick();
    const wrap = thumbsRefs.value[qIndex];
    if (!wrap) return;
    const last = (wrap.lastElementChild as HTMLElement | null) ?? wrap;
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    last.scrollIntoView({ block: "nearest", behavior: reduce ? "auto" : "smooth" });
  }

  function onFileChange(e: Event) {
    const input = e.target as HTMLInputElement;
    if (input.files) addFiles(input.files, pendingPickQ);
    input.value = "";
  }

  function removeImage(qIndex: number, index: number) {
    endOtherComposer(qIndex);
    imagesByQ.value[qIndex]?.splice(index, 1);
  }

  // DOM 级 drop 仅阻止默认（真正落盘走原生 onDragDropEvent，带落点坐标）。
  function onDrop(_e: DragEvent) {}

  /**
   * 落点坐标 → 命中的问题卡片索引，未命中返回 null。
   *
   * Tauri 把落点标注为 `PhysicalPosition`，但各平台底层给的单位并不一致：macOS 的
   * `draggingLocation()` 与 Linux 的 GTK 控件坐标都是逻辑像素（与 CSS 像素同尺度），只有
   * Windows 的 IDropTarget 给的是真·物理像素。所以先按平台选一种解释，再用另一种兜底——
   * 上游哪天统一了单位也不至于又只能拖到第一题。
   */
  function cardIndexAt(x: number, y: number): number | null {
    const el = document.elementFromPoint(x, y) as HTMLElement | null;
    const card = el?.closest?.(".q-card") as HTMLElement | null;
    if (card?.dataset.qIndex == null) return null;
    const idx = Number(card.dataset.qIndex);
    return Number.isNaN(idx) ? null : idx;
  }

  function questionAtPoint(x: number, y: number): number {
    if (!verticalMode.value) return current.value;
    const dpr = window.devicePixelRatio || 1;
    const candidates: [number, number][] = isWindows
      ? [
          [x / dpr, y / dpr],
          [x, y],
        ]
      : [
          [x, y],
          [x / dpr, y / dpr],
        ];
    for (const [cx, cy] of candidates) {
      const idx = cardIndexAt(cx, cy);
      if (idx != null) return idx;
    }
    return current.value;
  }

  const IMAGE_EXT = /\.(png|jpe?g|gif|webp|bmp|heic|heif|tiff?|svg)$/i;

  async function addDroppedPaths(paths: string[], qIndex: number) {
    if (selectOnly.value) return;
    endOtherComposer(qIndex);
    const attachPaths = new Set(attachments.value.map((a) => a.path));
    let addedImage = 0;
    for (const path of paths) {
      if (attachPaths.has(path)) continue;
      const name = path.split(/[\\/]/).pop() || "file";
      if (IMAGE_EXT.test(path)) {
        try {
          const data = await readImageDataUrl(path);
          const semi = data.indexOf(";");
          const mediaType = semi > 5 ? data.slice(5, semi) : "image/png";
          imagesByQ.value[qIndex]?.push({ data, mediaType, filename: name });
          addedImage++;
        } catch (err) {
          console.error("读取拖入图片失败", path, err);
        }
      } else if (!(replyFilesByQ.value[qIndex] ?? []).some((f) => f.path === path)) {
        replyFilesByQ.value[qIndex]?.push({ path, name });
      }
    }
    if (addedImage) scrollImagesIntoView(qIndex);
  }

  function removeReplyFile(qIndex: number, index: number) {
    endOtherComposer(qIndex);
    replyFilesByQ.value[qIndex]?.splice(index, 1);
  }

  // 粘贴图片：归到当前聚焦的问题（无聚焦则归当前题 active）。
  async function onPaste(e: ClipboardEvent) {
    if (selectOnly.value) return;
    const items = e.clipboardData?.items;
    if (!items) return;
    const files: File[] = [];
    for (let i = 0; i < items.length; i++) {
      const item = items[i];
      if (item.kind === "file" && item.type.startsWith("image/")) {
        const f = item.getAsFile();
        if (f) files.push(f);
      }
    }
    if (files.length) {
      e.preventDefault();
      await addFiles(files, focusedQ.value ?? current.value);
    }
  }

  // 输入框随内容自增高（封顶 240px，超出则框内滚动）。仅展开态生效（折叠态固定 1 行）。
  function autoGrow(i: number = current.value, allowShrink = true) {
    const el = inputRefs.value[i];
    resizeTextareaToContent(el, expandedQ(i), allowShrink);
  }

  // ===== Speech follows the unified action target, not passive scroll position. =====
  const speech = useSpeech({
    targetQuestion: actionQuestionIndex,
    inputByQ,
    inputRef,
    autoGrow,
  });

  // Toolbar actions are explicit question switches. Focus the owning textarea first so their
  // speech / attachment target cannot be inherited from an editor retained by passive scrolling.
  function focusQuestionAction(i: number) {
    if (speech.speechTargetQ.value !== i) speech.stopListening();
    endOtherComposer(i);
    setActive(i, false);
    focusComposer(i, true);
  }

  // textarea 聚焦/失焦：维护 focusedQ + 切当前题（聚焦即展开）；失焦且空则折叠。
  function onTextareaFocus(i: number) {
    if (speech.speechTargetQ.value !== i) speech.stopListening();
    const activation = programmaticFocusActivation;
    const manuallyActivated =
      activation?.qIndex === i ? activation.manuallyActivated : false;
    if (activation?.qIndex === i) programmaticFocusActivation = null;
    activateComposer(i, manuallyActivated);
    focusedQ.value = i;
    setActive(i, false);
    nextTick(() => autoGrow(i));
  }
  function onComposerInput(i: number, event: Event) {
    activateComposer(i);
    autoGrow(i, inputMayShrinkTextarea(event));
  }
  function onComposerMouseDown(i: number) {
    activateComposer(i);
    speech.onTextareaMouseDown();
  }
  function onTextareaBlur(i: number) {
    rememberComposerSelection(i);
    if (focusedQ.value === i) focusedQ.value = null;
    nextTick(() => {
      settleTextareaHeightAfterBlur(inputRefs.value[i], expandedQ(i));
    });
  }

  // ===== 多题导航（纵向列表：当前题指针 + 滚动定位） =====
  function markVisited(i: number) {
    if (i >= 0 && i < visited.value.length) visited.value[i] = true;
  }

  // 第 i 题是否已完整落在视口内（含上下 16px 呼吸位）；卡片比视口还高时永远返回 false（无法完整容纳）。
  function isCardFullyVisible(i: number): boolean {
    const root = contentRef.value;
    const el = cardRefs.value[i];
    if (!root || !el) return true;
    const margin = 16;
    const rootRect = root.getBoundingClientRect();
    const elRect = el.getBoundingClientRect();
    return (
      elRect.top >= rootRect.top + margin &&
      elRect.bottom <= rootRect.top + root.clientHeight - margin
    );
  }

  // 第 i 题是否**完全在屏外**（整卡在视口上方或下方，一点都看不到）。用于 reveal-first：刚打开时长 message
  // 把 Q1 顶到屏外，此时点「下一个」应先把 Q1 露出来而非跳到 Q2；而只是「底部被切一点」不算屏外，可正常推进。
  function isCardOffScreen(i: number): boolean {
    const root = contentRef.value;
    const el = cardRefs.value[i];
    if (!root || !el) return false;
    const rootRect = root.getBoundingClientRect();
    const elRect = el.getBoundingClientRect();
    return (
      elRect.top >= rootRect.top + root.clientHeight || elRect.bottom <= rootRect.top
    );
  }

  function deactivateOffscreenComposer() {
    const i = focusedQ.value;
    if (i === null) return;
    if (
      !shouldDeactivateOffscreenComposer(
        i,
        dockedComposerQ.value,
        isCardOffScreen(i)
      )
    ) {
      return;
    }
    if (speech.speechTargetQ.value === i) speech.stopListening();
    inputRefs.value[i]?.blur();
    if (composerOwnerQ.value === i) clearComposerOwner();
  }

  // 把内容滚到最顶（scrollTop=0）：纵向模式在首题按「上一个」时用来完整露出上方 message。
  function scrollContentToTop() {
    const root = contentRef.value;
    if (!root) return;
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    root.scrollTo({ top: 0, behavior: reduce ? "auto" : "smooth" });
  }

  function returnComposerHome() {
    const i = composerOwnerQ.value;
    const root = contentRef.value;
    const anchor = i === null ? null : composerAnchorRefs.value[i];
    if (i === null || !root || !anchor) return;
    if (verticalMode.value) setActive(i, false);
    const rootRect = root.getBoundingClientRect();
    const anchorRect = anchor.getBoundingClientRect();
    const offsetTop = anchorRect.top - rootRect.top + root.scrollTop;
    const homeHeight = composerHomeHeights[i] ?? 0;
    const max = Math.max(0, root.scrollHeight - root.clientHeight);
    const top = Math.max(
      0,
      Math.min(max, offsetTop + homeHeight - root.clientHeight + 16)
    );
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    returnFocusQ = i;
    activeLockUntil = Date.now() + NAV_LOCK_MS;
    root.scrollTo({ top, behavior: reduce ? "auto" : "smooth" });
    scheduleScrollWork();
  }

  // 把第 i 题滚到「顶部对齐 + 16px 呼吸位」——统一露出该题**顶部**（含末题：夹到 max 即贴底、也完整可见）。
  // 不再用比例定位（会把靠后的题只露出个顶、后续再切就卡住）。对齐顶部后阅读线不落在目标题 → 必须靠 NAV_LOCK_MS
  // 锁住 scroll-spy 到动画结束，避免 current 被抢；用户手动滚动时 scroll-spy 才重新接管。
  function scrollQuestionIntoView(i: number) {
    const root = contentRef.value;
    const el = cardRefs.value[i];
    if (!root || !el) return;
    const max = root.scrollHeight - root.clientHeight;
    if (max <= 0) return; // 内容未超视口：无可滚动空间（active 由 setActive 直接置位）
    // 已完整可见（含上下 16px 呼吸位）就**不滚动**：避免「目标题本就在屏内还硬滚一段」（用户反馈）。
    if (isCardFullyVisible(i)) return;
    const rootRect = root.getBoundingClientRect();
    const elRect = el.getBoundingClientRect();
    const offsetTop = elRect.top - rootRect.top + root.scrollTop;
    const top = Math.max(0, Math.min(max, offsetTop - 16));
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    root.scrollTo({ top, behavior: reduce ? "auto" : "smooth" });
  }

  // 设当前题：夹边界、置 active + 标记已看到；可选滚动到可见。短锁避免滚动事件回改。
  function setActive(index: number, scroll: boolean) {
    const i = Math.max(0, Math.min(index, total.value - 1));
    current.value = i;
    markVisited(i);
    if (scroll) {
      activeLockUntil = Date.now() + NAV_LOCK_MS;
      scrollQuestionIntoView(i);
    }
  }

  // 导航「整题闪一下渐隐」：提示焦点落点，上一个/下一个按钮与 ⌘[/⌘]/⌘↵ 都会触发。闪光区域为 `.q-card::before`
  // （圆角、accent 淡底，见 CSS），一次性 opacity 1→0 渐隐。
  // 用 class + 强制 reflow 重放（卡片无 :class 绑定，Vue 不会清掉）；prefers-reduced-motion 下不闪。
  function flashCard(i: number) {
    const el = cardRefs.value[i];
    if (!el) return;
    el.classList.remove("flash");
    void el.offsetWidth; // 强制 reflow，令同一题也能再次触发动画
    el.classList.add("flash");
  }

  // 跳到第 i 题（绝对索引）：置当前题 + 标记看过 + **自动聚焦该题输入框**（用户要求「切到某题就激活它的输入框」）。
  // 聚焦会触发折叠输入框展开、改变高度，故**展开后再滚动**（双 nextTick），避免用旧高度定位。全程用 NAV_LOCK_MS
  // 锁住 scroll-spy 到滚动动画结束，避免 current 被滚动事件抢走。供上一个/下一个、⌘[/⌘]、⌘↵ 复用，行为一致。
  function goToIdx(target: number) {
    const i = Math.max(0, Math.min(target, total.value - 1));
    if (
      actionQuestionIndex.value !== i ||
      (composerOwnerQ.value !== null && composerOwnerQ.value !== i)
    ) {
      speech.stopListening();
    }
    endOtherComposer(i);
    setActive(i, false); // 先置当前题、不滚动（滚动放到展开之后）
    activeLockUntil = Date.now() + NAV_LOCK_MS;
    nextTick(() => {
      focusComposer(i, true);
      nextTick(() => {
        activeLockUntil = Date.now() + NAV_LOCK_MS; // 展开后重置锁：确保覆盖此刻才开始的 smooth 滚动动画
        scrollQuestionIntoView(i);
      });
    });
  }

  // Move relative to the unified action target and let goToIdx transfer focus before scrolling.
  function goRel(delta: number) {
    goToIdx(actionQuestionIndex.value + delta);
  }

  // 旧版顺序模式切题：仅一题可见，改 current 即换页（聚焦/滚动由 Transition after-enter 处理）。
  function goToSeq(index: number) {
    if (index < 0 || index >= total.value || index === current.value) return;
    speech.stopListening(); // 切题前停语音，避免回调写进旧题
    clearComposerOwner();
    nextSequentialFocusIsManual = true;
    current.value = index;
    markVisited(index);
  }

  // 旧版单题面板头部滚到顶（Message 很长时也能露出当前题）。
  function scrollHeaderIntoView() {
    const el = qHeaderRef.value;
    if (!el) return;
    const reduce = window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
    el.scrollIntoView({ block: "start", behavior: reduce ? "auto" : "smooth" });
  }

  // 旧版切题动画完成后：聚焦输入 + 校正高度 + 滚动头部到顶（新面板已挂载、高度确定）。
  function onQuestionEntered() {
    if (verticalMode.value) return;
    if (suppressNextSeqFocus) {
      suppressNextSeqFocus = false;
      autoGrow(current.value);
      scrollHeaderIntoView();
      find.refreshFind();
      pendingFindRevealResolve?.();
      pendingFindRevealResolve = null;
      return;
    }
    if (nextSequentialFocusIsManual) focusComposer(current.value, true);
    else focusComposerIfInitiallyVisible(current.value);
    nextSequentialFocusIsManual = false;
    autoGrow(current.value);
    scrollHeaderIntoView();
    pendingFindRevealResolve?.();
    pendingFindRevealResolve = null;
  }

  // 纵向导航统一在此闪一下：无论来自「上一个/下一个」按钮还是 ⌘[/⌘]，落点题都整题闪一次（用户要求按钮也闪）。
  function goPrev() {
    if (verticalMode.value) {
      // 已在首题：「上一个」= 把上方 message 完整露出（滚到最顶），而非无动作（用户预期两级：Q2→Q1 露出 Q1、
      // 在 Q1 再上一个才露出 message）。
      if (actionQuestionIndex.value === 0) {
        activeLockUntil = Date.now() + NAV_LOCK_MS;
        scrollContentToTop();
        flashCard(0);
        return;
      }
      goRel(-1);
      flashCard(current.value);
      return;
    }
    slideDir.value = "prev";
    goToSeq(current.value - 1);
  }

  function goNext() {
    if (verticalMode.value) {
      // 当前题**完全在屏外**（如长 message 刚打开把 Q1 顶到屏外）→ 先把它露出来 + 聚焦 + 闪一下，而非直接跳下一题
      // （用户反馈：刚打开点「下一个」直接跳到 Q2、Q1 从没露出）。仅「底部被切一点」不算屏外，可正常推进到下一题。
      const from = actionQuestionIndex.value;
      if (isCardOffScreen(from)) {
        goToIdx(from);
        flashCard(from);
        return;
      }
      goRel(1);
      flashCard(current.value);
      return;
    }
    slideDir.value = "next";
    goToSeq(current.value + 1);
  }

  // 从 from 之后找第一个「未答」/「未看过」的题；无则 -1（供 ⌘↵ 智能推进）。
  function nextUnansweredAfter(from: number): number {
    for (let i = from + 1; i < total.value; i++) if (!isAnswered(i)) return i;
    return -1;
  }
  function nextUnseenAfter(from: number): number {
    for (let i = from + 1; i < total.value; i++) if (!(visited.value[i] ?? false)) return i;
    return -1;
  }

  // 纵向模式的 ⌘↵：从当前焦点向后找下一个未答题→跳过去（闪+滚入+置焦点）；后面没有未答了→已看完全部
  // 则提交，否则去下一个没看过的题继续读（推进「读完」门槛）。永不落到已答题（修「跳回已答」bug）。
  // 自由模式想留空提交：直接点底部「提交」按钮（阶段②/③ 可见）。
  function onCmdEnter() {
    const from = cmdEnterFromQ.value;
    // 当前题**完全在屏外**（长 message 顶开首题等）→ 先把它露出来 + 聚焦 + 闪一下，而非直接跳到后面的未答题。
    // 但若该题输入框正在底部固定区获得焦点，编辑器已经可见，应直接继续 / 提交，不能跳回题卡。
    if (
      shouldRevealQuestionBeforeCmdEnter(
        from,
        focusedQ.value,
        dockedComposerQ.value,
        isCardOffScreen(from)
      )
    ) {
      goToIdx(from);
      flashCard(from);
      return;
    }
    const u = nextUnansweredAfter(from);
    if (u >= 0) {
      goToIdx(u);
      flashCard(u);
      return;
    }
    if (lastSeen.value) {
      submit();
      return;
    }
    const s = nextUnseenAfter(from);
    if (s >= 0) {
      goToIdx(s);
      flashCard(s);
      return;
    }
    submit();
  }

  function collectAnswers(): QuestionAnswer[] {
    return questions.value.map((q, i) => {
      const answer: QuestionAnswer = {
        selectedOptions: q.predefinedOptions
          .map((o) => o.text)
          .filter((o) => (chosenByQ.value[i] ?? []).includes(o)),
        userInput: inputByQ.value[i] ?? "",
        images: imagesByQ.value[i] ?? [],
        files: (replyFilesByQ.value[i] ?? []).map((f) => f.path),
      };
      // Local whats-next TODO options refresh after the daemon snapshots its request. Submit a
      // refreshed TODO as raw task text + stable id so output and dequeue semantics stay exact.
      const whatsNextTodo =
        i === 0 && whatsNext.value
          ? selectedWhatsNextTodo(
              q.predefinedOptions,
              chosenByQ.value[i] ?? [],
              todos.value,
              t("popup.todos.optionPrefix"),
            )
          : null;
      if (whatsNextTodo) {
        answer.selectedOptions = answer.selectedOptions.filter(
          (text) => text !== whatsNextTodo.optionText,
        );
        answer.userInput = [whatsNextTodo.text, answer.userInput.trim()]
          .filter((text) => text.trim())
          .join("\n\n");
        answer.todoIds = [whatsNextTodo.id];
        answer.todoSelections = [
          { id: whatsNextTodo.id, attachments: whatsNextTodo.attachments },
        ];
      }
      // 待办下拉区选中的条目（恒归**最后一题**，spec D7 第 11 轮改版）：每条加「另外看一下
      // 这个待办任务：」前缀并入 userInput（手输文本在前、待办在后，空行分隔），id 送后端出队。
      if (
        i === total.value - 1 &&
        todoChipsEnabled.value &&
        selectedTodos.value.length
      ) {
        answer.userInput = [
          answer.userInput,
          ...selectedTodos.value.map((td) =>
            t("popup.todos.mergeLine", { text: td.text })
          ),
        ]
          .filter((s) => s.trim())
          .join("\n\n");
        answer.todoIds = selectedTodos.value.map((td) => td.id);
        answer.todoSelections = selectedTodos.value.map((td) => ({
          id: td.id,
          attachments: (td.attachments ?? []).map((attachment) => ({
            id: attachment.id,
            name: attachment.name,
            path: attachment.path,
            sourcePath: attachment.sourcePath,
            storage: attachment.storage,
          })),
        }));
      }
      return answer;
    });
  }

  async function submit() {
    if (submitting.value || !canSubmit.value) return;
    submitting.value = true;
    attach.stopPreview();
    try {
      await submitPopup({ answers: collectAnswers() });
    } catch {
      submitting.value = false;
    }
  }

  const selectedConfirmChoice = computed(() =>
    confirmChoiceIndex.value === null
      ? null
      : confirmRequest.value?.choices[confirmChoiceIndex.value] ?? null
  );
  const confirmInput = computed(() => confirmRequest.value?.presentation.input ?? null);
  const showConfirmInput = computed(
    () =>
      !!confirmInput.value &&
      selectedConfirmChoice.value?.id === confirmInput.value.visibleWhenActionId
  );
  const confirmCanSubmit = computed(
    () => confirmChoiceIndex.value !== null && !submitting.value
  );
  const confirmDetailHtml = computed(() =>
    confirmRequest.value?.detail.bodyMd
      ? renderMarkdown(confirmRequest.value.detail.bodyMd, codeCopyLabels.value)
      : ""
  );
  const confirmToolName = computed(
    () =>
      confirmRequest.value?.context.find((field) => field.id === "tool")?.value ??
      "Tool"
  );

  // ===== 前缀档位选择器（D51）=====
  // 同 group 的 choices 是同一动作的不同泛化档位：折叠成一行展示，所有 group 共享
  // 一个档位选择器；提交仍然走被选中 choice 的 wire index，协议零改动。
  const confirmVariantLevel = ref(0);

  const confirmVariantGroups = computed(() => {
    const groups = new Map<
      string,
      {
        level: number;
        index: number;
        levelLabel: string;
        segmentLabel: string;
        recommended: boolean;
      }[]
    >();
    (confirmRequest.value?.choices ?? []).forEach((choice, index) => {
      const variant = choice.variant;
      if (!variant) return;
      const list = groups.get(variant.group) ?? [];
      list.push({
        level: variant.level,
        index,
        levelLabel: variant.levelLabel,
        segmentLabel: variant.segmentLabel || variant.levelLabel,
        recommended: variant.recommended,
      });
      groups.set(variant.group, list);
    });
    for (const list of groups.values()) list.sort((a, b) => a.level - b.level);
    return groups;
  });

  // 选择器档位（标签取第一个 group；各 group 的阶梯由 Hook 端保证一致）。
  const confirmVariantLevels = computed(() => {
    const first:
      | {
          level: number;
          levelLabel: string;
          segmentLabel: string;
          recommended: boolean;
        }[]
      | undefined =
      confirmVariantGroups.value.values().next().value;
    return first && first.length > 1
      ? first.map((entry) => ({
          level: entry.level,
          label: entry.segmentLabel,
          prefixLabel: entry.levelLabel,
          recommended: entry.recommended,
        }))
      : [];
  });

  const confirmVariantRecommendedLevel = computed(
    () =>
      confirmVariantLevels.value.find((entry) => entry.recommended)?.level ??
      confirmVariantLevels.value[0]?.level ??
      0,
  );

  // 展示行：普通 choice 原样一行；每个 group 在其首个 choice 的位置折叠为一行，
  // 行内容（label/description/wire index）跟随当前档位。
  const confirmRows = computed(() => {
    const choices = confirmRequest.value?.choices ?? [];
    const rows: { index: number; choice: ConfirmChoice; group?: string }[] = [];
    const seen = new Set<string>();
    choices.forEach((choice, index) => {
      const variant = choice.variant;
      if (!variant) {
        rows.push({ index, choice });
        return;
      }
      if (seen.has(variant.group)) return;
      seen.add(variant.group);
      const entries = confirmVariantGroups.value.get(variant.group) ?? [];
      const active =
        entries.find((entry) => entry.level === confirmVariantLevel.value) ??
        entries[0];
      if (active) {
        rows.push({ index: active.index, choice: choices[active.index], group: variant.group });
      }
    });
    return rows;
  });

  function selectConfirmVariantLevel(level: number) {
    if (submitting.value || level === confirmVariantLevel.value) return;
    const previousGroup = selectedConfirmChoice.value?.variant?.group;
    confirmVariantLevel.value = level;
    // 已选中某个档位行时，选中项跟随切到同组的新档位。
    if (previousGroup) {
      const entries = confirmVariantGroups.value.get(previousGroup) ?? [];
      const target = entries.find((entry) => entry.level === level);
      if (target) confirmChoiceIndex.value = target.index;
    }
  }

  // ===== In-page find (⌘/Ctrl+F) — see docs/specs/popup-find.md =====
  const confirmChoiceTexts = computed(() =>
    confirmRows.value.map((row) => {
      const d = row.choice.description?.trim();
      return d ? `${row.choice.label}\n${d}` : row.choice.label;
    }),
  );
  const confirmTitle = computed(() => confirmRequest.value?.title ?? "");
  const confirmSummary = computed(
    () => confirmRequest.value?.detail.summary ?? "",
  );
  const confirmBodyText = computed(
    () => confirmRequest.value?.detail.bodyMd ?? "",
  );

  async function revealQuestionForFind(index: number): Promise<void> {
    const i = Math.max(0, Math.min(index, total.value - 1));
    if (verticalMode.value) {
      setActive(i, true);
      await nextTick();
      return;
    }
    if (i === current.value) {
      scrollHeaderIntoView();
      await nextTick();
      return;
    }
    speech.stopListening();
    clearComposerOwner();
    suppressNextSeqFocus = true;
    nextSequentialFocusIsManual = false;
    slideDir.value = i > current.value ? "next" : "prev";
    current.value = i;
    markVisited(i);
    await new Promise<void>((resolve) => {
      pendingFindRevealResolve = resolve;
      window.setTimeout(() => {
        if (pendingFindRevealResolve === resolve) {
          pendingFindRevealResolve = null;
          resolve();
        }
      }, 400);
    });
  }

  const find = usePopupFind({
    contentRef,
    isConfirm,
    confirmRequest,
    messageText,
    viewSource,
    attachments,
    questions,
    whatsNext,
    todoPrefix: computed(() => t("popup.todos.optionPrefix")),
    confirmChoiceTexts,
    confirmTitle,
    confirmSummary,
    confirmToolName,
    confirmBodyText,
    currentQ: current,
    verticalMode,
    revealQuestion: revealQuestionForFind,
  });

  function startPermissionDiffEnrichment() {
    const edit = permissionEdit.value;
    const id = confirmRequest.value?.id;
    if (!edit || !id || edit.operation.type === "unsupported") return;
    permissionDiffLoading.value = true;
    perfMarkFe("permission_diff.worker_start");
    void enrichPermissionDiff(id)
      .then((model) => {
        if (confirmRequest.value?.id === id && model.requestId === id) {
          permissionDiff.value = model;
        }
      })
      .catch(() => {})
      .finally(() => {
        if (confirmRequest.value?.id === id) permissionDiffLoading.value = false;
      });
  }

  function selectConfirmChoice(index: number) {
    if (!submitting.value) confirmChoiceIndex.value = index;
  }

  async function submitConfirm() {
    if (!confirmCanSubmit.value || confirmChoiceIndex.value === null) return;
    submitting.value = true;
    const comment = showConfirmInput.value
      ? confirmComment.value.slice(0, confirmInput.value?.maxChars ?? 1000)
      : null;
    try {
      await submitConfirmAction(confirmChoiceIndex.value, comment || null);
    } catch {
      submitting.value = false;
    }
  }

  function requestConfirmClose() {
    if (!submitting.value) showConfirmCloseWarning.value = true;
  }

  function dismissConfirmCloseWarning() {
    showConfirmCloseWarning.value = false;
  }

  async function confirmCloseAndDeny() {
    const req = confirmRequest.value;
    if (!req || submitting.value) return;
    const index = req.choices.findIndex((choice) => choice.id === req.dismissActionId);
    if (index < 0) return;
    confirmChoiceIndex.value = index;
    showConfirmCloseWarning.value = false;
    await submitConfirm();
  }

  // 取消入口：有回答时二次确认，否则直接取消。
  function requestCancel() {
    if (submitting.value) return;
    if (hasAnyAnswer.value) {
      showCancelConfirm.value = true;
    } else {
      doCancel();
    }
  }

  async function doCancel() {
    if (submitting.value) return;
    submitting.value = true;
    showCancelConfirm.value = false;
    attach.stopPreview();
    try {
      await cancelPopup();
    } catch {
      submitting.value = false;
    }
  }

  function dismissCancelConfirm() {
    showCancelConfirm.value = false;
  }

  // 仅「纯」⌘/Ctrl（未叠加 Shift/Option）才算命中快捷键修饰键：例如 Cmd+Shift（截屏）下
  // 再按 1–9 不会命中选项快捷键，故此时不应高亮。
  function onlyCmdHeld(e: KeyboardEvent): boolean {
    return primaryModifierPressed(e) && !e.shiftKey && !e.altKey;
  }

  /** Insert a newline at the caret of the focused textarea (enter-submit mode). */
  function insertNewlineInFocusedTextarea(): void {
    const el = document.activeElement;
    if (!(el instanceof HTMLTextAreaElement)) return;
    const start = el.selectionStart ?? el.value.length;
    const end = el.selectionEnd ?? start;
    const value = el.value;
    const next = value.slice(0, start) + "\n" + value.slice(end);
    // Prefer native input setter so Vue v-model picks up the change.
    const proto = Object.getOwnPropertyDescriptor(
      HTMLTextAreaElement.prototype,
      "value"
    );
    proto?.set?.call(el, next);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    const caret = start + 1;
    el.setSelectionRange(caret, caret);
  }

  // ⌘/Ctrl 按下/松开 → 切换 cmdHeld（驱动快捷键 Badge 高亮）。窗口失焦时复位，避免卡住。
  function onKeyup(e: KeyboardEvent) {
    cmdHeld.value = onlyCmdHeld(e);
  }
  function onWindowBlur() {
    cmdHeld.value = false;
  }

  function onKeydown(e: KeyboardEvent) {
    const mod = primaryModifierPressed(e);
    cmdHeld.value = onlyCmdHeld(e);
    // In-page find (⌘/Ctrl+F, Esc while open, ⌘G, …) — before business shortcuts.
    if (find.handleFindKeydown(e)) return;
    if (isConfirm.value) {
      if (e.key === "Escape") {
        e.preventDefault();
        if (showConfirmCloseWarning.value) dismissConfirmCloseWarning();
        else requestConfirmClose();
        return;
      }
      if (e.key === "Enter") {
        if (e.isComposing || (e as KeyboardEvent & { keyCode?: number }).keyCode === 229) {
          return;
        }
        const anyMod = e.metaKey || e.ctrlKey || e.shiftKey || e.altKey;
        const isPrimarySendMod = mod && !e.shiftKey && !e.altKey;
        const shouldSubmit = submitWithBareEnter.value ? !anyMod : isPrimarySendMod;
        if (shouldSubmit) {
          e.preventDefault();
          void submitConfirm();
          return;
        }
        // Non-submit Enter (bare in cmdEnter mode, or mod+Enter in enter mode): no special
        // confirm UI for newlines — just ignore.
        if (submitWithBareEnter.value && anyMod) {
          e.preventDefault();
        }
        return;
      }
      if (mod && (e.key === "w" || e.key === "W")) {
        e.preventDefault();
        if (Date.now() >= appearGuardUntil) requestConfirmClose();
        return;
      }
      if (mod && e.key >= "1" && e.key <= "9") {
        // 快捷键按展示行计数（档位 group 折叠为一行，D51）。
        const row = confirmRows.value[Number(e.key) - 1];
        if (row) {
          e.preventDefault();
          selectConfirmChoice(row.index);
        }
        return;
      }
      return;
    }
    // 录音中按 Esc：结束本次语音输入（不关闭弹窗）。
    if (e.key === "Escape" && speech.listening.value) {
      e.preventDefault();
      speech.stopListening();
      return;
    }
    if (e.key === "Enter") {
      // IME composing: never treat Enter as submit (matches todo add input).
      if (e.isComposing || (e as KeyboardEvent & { keyCode?: number }).keyCode === 229) {
        return;
      }
      const anyMod = e.metaKey || e.ctrlKey || e.shiftKey || e.altKey;
      const isPrimarySendMod = mod && !e.shiftKey && !e.altKey; // pure ⌘/Ctrl+Enter
      const shouldSubmit = submitWithBareEnter.value
        ? !anyMod
        : isPrimarySendMod;
      const shouldNewline =
        submitWithBareEnter.value && anyMod
          ? true
          : !submitWithBareEnter.value && !isPrimarySendMod && anyMod
            ? true // e.g. Shift/Option+Enter while in cmdEnter mode: keep as newline
            : false;

      if (shouldNewline) {
        // Insert a newline into the focused textarea (any-mod+Enter in enter mode).
        e.preventDefault();
        insertNewlineInFocusedTextarea();
        return;
      }
      if (shouldSubmit) {
        e.preventDefault();
        // 纵向多题：发送快捷键智能推进（下一未答→看完提交→否则去下一未看），永不回跳已答。
        if (verticalMode.value) {
          onCmdEnter();
        } else if (isMulti.value && !onLastQuestion.value) {
          // 旧版顺序多题：非最后一题始终前往下一题（即使提交按钮已出现），最后一题才提交。
          goNext();
        } else {
          submit();
        }
        return;
      }
      // cmdEnter mode + bare Enter: let the browser insert a newline in the textarea.
    }
    if (mod && (e.key === "w" || e.key === "W")) {
      e.preventDefault();
      // 出现动画期内吞掉 ⌘W：防止用户为关闭其它窗口按下的 ⌘W 恰好误关刚弹出的弹窗（仍 preventDefault 拦住原生关窗）。
      if (Date.now() < appearGuardUntil) return;
      requestCancel();
      return;
    }
    // 语音输入快捷键（可在设置中自定义；空串=关闭）。
    if (
      speech.speechSupported.value &&
      speech.speechShortcut.value &&
      matchShortcut(e, speech.speechShortcut.value)
    ) {
      e.preventDefault();
      speech.toggleSpeech();
      return;
    }
    // 多题：CMD+] 下一题，CMD+[ 上一题（不影响 CMD+回车）。闪一下由 goNext/goPrev 内部统一处理（按钮/键盘一致）。
    if (isMulti.value && mod && e.key === "]") {
      e.preventDefault();
      goNext();
      return;
    }
    if (isMulti.value && mod && e.key === "[") {
      e.preventDefault();
      goPrev();
      return;
    }
    // CMD+数字（1-9）：选中/取消当前题对应序号的选项。
    if (mod && e.key >= "1" && e.key <= "9") {
      const idx = Number(e.key) - 1;
      const opts = actionQuestion.value?.predefinedOptions;
      if (opts && idx < opts.length && idx < OPTION_HOTKEY_MAX) {
        e.preventDefault();
        toggleByIndex(idx);
        return;
      }
    }
    const tgt = e.target as HTMLElement | null;
    const typing =
      tgt && (tgt.tagName === "TEXTAREA" || tgt.tagName === "INPUT");
    if (!typing) attach.handleAttachmentKey(e);
  }

  // 方案6 预热：领用一次性守卫——首个带 request 的 init 才渲染，避免重复领用。
  let adopting = false;
  let interactionRendered = false;

  // 把（含 request 的）init 渲染上屏：套主题/语言/来源 → 设 request → 报告 ready → 双 rAF 打点。
  // daemon 收到 ready 后统一决定前景显示或后方级联；冷/热 helper 都保持隐藏到该授权到达。
  function renderInit(init: PopupInit) {
    const interaction = init.interaction;
    if (!interaction || interactionRendered) return;
    interactionRendered = true;
    applyTheme(init.theme);
    // 精确语言来自 popup_init（零钥匙串）；main.ts 只做 auto 兜底，故此处校正。
    if (typeof init.language === "string") applyLanguage(init.language);
    pinned.value = init.alwaysOnTop;
    sourceName.value = init.sourceName;
    projectName.value = init.projectName;
    projectPath.value = init.project;
    agentKind.value = init.agentKind ?? "";
    agentPid.value = init.agentPid ?? null;
    agentConsoleSessionId.value = init.agentConsoleSessionId ?? "";
    createdAtMs.value = init.createdAtMs ?? 0;
    // 领用/渲染即刻校准 now，避免 tick 首帧前相对时间偏大。
    nowMs.value = Date.now();
    // 语音设置改取自 popup_init（不再走 get_settings / 钥匙串）。
    speech.speechLang.value = init.speechLanguage || "auto";
    speech.speechShortcut.value = init.speechShortcut || "cmd+d";
    if (init.popupSubmitKey === "enter" || init.popupSubmitKey === "cmdEnter") {
      popupSubmitKey.value = init.popupSubmitKey;
    }
    verticalEnabled.value = init.verticalQuestions ?? false;
    const req = interaction.type === "ask" ? interaction.request : null;
    request.value = req;
    const whatsNextQuestion = req?.whatsNext ? req.questions[0] : null;
    whatsNextStaticOptions = whatsNextQuestion
      ? whatsNextQuestion.predefinedOptions.filter((option) => !option.todoId)
      : [];
    whatsNextBaseMessage = whatsNextQuestion?.message.split("\n\n", 1)[0] ?? "";
    confirmRequest.value = interaction.type === "confirm" ? interaction.request : null;
    permissionEdit.value = interaction.type === "confirm" ? init.popupEdit ?? null : null;
    permissionDiff.value = permissionEdit.value?.initialDiff
      ? {
          ...permissionEdit.value.initialDiff,
          requestId: interaction.type === "confirm" ? interaction.request.id : "",
        }
      : null;
    permissionDiffLoading.value = false;
    confirmChoiceIndex.value =
      interaction.type === "confirm" && interaction.request.presentation.defaultActionId
        ? interaction.request.choices.findIndex(
            (choice) => choice.id === interaction.request.presentation.defaultActionId
          )
        : null;
    if (confirmChoiceIndex.value === -1) confirmChoiceIndex.value = null;
    // 档位选择器默认停在推荐档（D51）。
    confirmVariantLevel.value =
      interaction.type === "confirm"
        ? interaction.request.choices.find((choice) => choice.variant?.recommended)
            ?.variant?.level ?? 0
        : 0;
    confirmComment.value = "";
    showConfirmCloseWarning.value = false;
    const n = req?.questions.length ?? 0;
    resetComposerDock();
    chosenByQ.value = Array.from({ length: n }, () => []);
    inputByQ.value = Array.from({ length: n }, () => "");
    imagesByQ.value = Array.from({ length: n }, () => []);
    replyFilesByQ.value = Array.from({ length: n }, () => []);
    visited.value = Array.from({ length: n }, () => false);
    if (n > 0) visited.value[0] = true;
    // 重置每题 DOM 引用数组（v-for 重渲染会按索引回填）。
    inputRefs.value = [];
    cardRefs.value = [];
    sentinelRefs.value = [];
    thumbsRefs.value = [];
    focusedQ.value = null;
    current.value = 0;
    todos.value = [];
    todosOpen.value = false;
    todoChosenIds.value = [];
    todoChosenSnapshots.value = {};
    attach.loadThumbs();
    attach.loadDragIcons();
    // 纵向模式（实验开关开 且 多题）：不自动聚焦、保持全部折叠、建哨兵观察。
    // 否则（单题 / 旧版顺序模式）：聚焦当前题输入框 + 校正高度。
    const vertical = verticalEnabled.value && n > 1;
    const afterPaint = () => {
      // DOM 更新后再聚焦/建观察（此时 textarea / 哨兵已挂载）。
      nextTick(() => {
        if (!vertical) {
          focusComposerIfInitiallyVisible(0);
          autoGrow(0);
        } else {
          setupQuestionObserver();
        }
      });
      // 双 rAF：第一帧让正文进入 DOM 并即将绘制，第二帧回调时该帧已真正合成上屏，
      // 此刻打点更贴近用户真正看到内容的时刻（比单 rAF 晚约 1 帧）。
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          perfMarkFe("fe.painted");
          if (isConfirm.value) confirmPopupReady().catch(() => {});
          startPermissionDiffEnrichment();
          // harness 模式：内容已上屏即自动取消，免人工点按。
          if (init.perfAutodismiss && !isConfirm.value) {
            cancelPopup().catch(() => {});
          }
        });
      });
    };
    // Hidden windows have no display link, so readiness must not wait for rAF. nextTick is enough
    // to put this request into the DOM; daemon presentation restores rAF for afterPaint.
    nextTick(() => {
      popupShowWindow().catch(() => {});
      appearGuardUntil = Date.now() + APPEAR_GUARD_MS;
      afterPaint();
    });
    // 内容已渲染：把其余初始化（事件监听 / 语音 / 自更新 / 终端探测）放到首帧之后，不阻塞首屏。
    void initAfterPaint(init);
  }

  // 预热弹窗领用：重新 pull popup_init，若已带 request 则渲染（一次性）。
  async function adopt() {
    if (request.value || confirmRequest.value || adopting) return;
    adopting = true;
    try {
      const init = await popupInit();
      if (init.interaction) {
        // 热路径领用：丢弃预热阶段缓存的标记（fe.bootstrap/fe.mounted/待命 popup_init 不属本次请求），
        // 只上报领用后的标记（如 fe.painted），避免污染时间线（负的 page boot）。
        if (init.perf) perfEnableFe(true);
        renderInit(init);
      }
    } catch (err) {
      console.error("popup adopt 失败", err);
    } finally {
      adopting = false;
    }
  }

  onMounted(async () => {
    // 头部相对时间每秒走字（开销极小；无 createdAt 时 computed 返回空，不渲染）。
    timeTicker = window.setInterval(() => {
      nowMs.value = Date.now();
    }, 1000);
    // 同步窗口监听（开销极小）：放最前，保证粘贴 / 快捷键 / Esc 从首帧即可用。
    window.addEventListener("paste", onPaste);
    window.addEventListener("keydown", onKeydown);
    window.addEventListener("keyup", onKeyup);
    window.addEventListener("blur", onWindowBlur);
    document.addEventListener("mouseup", speech.onDocMouseUp);
    // 方案6：预热弹窗领用唤醒事件——尽早注册以免漏接（冷路径不会收到，无害）。
    unlistenShow = await listen("popup-show", () => {
      void adopt();
    });
    // 关键路径：第一步即取请求内容并渲染，尽快上屏；其余初始化全部移到渲染之后（见 initAfterPaint）。
    try {
      const init = await popupInit();
      // 后端在 helper 进程收到 ASKHUMAN_PERF_ID 时置 perf=true：开启前端埋点并冲刷此前缓存的标记。
      if (init.perf) perfEnableFe();
      perfMarkFe("fe.popup_init_done");
      if (init.interaction) {
        // 冷路径 / 已领用：直接渲染。
        renderInit(init);
      } else {
        // 预热待命：先按当前主题/语言渲染（窗口隐藏），等 popup-show 领用。
        applyTheme(init.theme);
        if (typeof init.language === "string") applyLanguage(init.language);
        // 兜底竞态：领用可能发生在首个 popup_init 与监听注册之间，立即复查一次。
        void adopt();
      }
    } catch (err) {
      console.error("popup_init 失败", err);
      loadError.value = String(err);
    }
  });

  // 首帧渲染后再执行的非关键初始化。这些监听对应的事件都由 daemon 在 show 之后才可能发来，
  // 或为用户 / 托盘触发，略晚于首帧注册无碍（自更新态另用 popupUpdateState() 拉初值兜底）。
  // 放此处是为了不阻塞弹窗首屏（原先这些 await 串在 popupInit 之前，正是「加载中」停留的来源）。
  async function initAfterPaint(init: PopupInit) {
    // Any process may update todos.json while the question stays open. The popup host watches
    // the file and emits this event to refresh the ordinary todo section or whats-next options.
    unlistenTodos = await listen("todos-updated", () => {
      void loadTodos();
    });
    void loadTodos();
    await attach.initAttachmentPreviewListeners();
    unlistenDrop = await getCurrentWebview().onDragDropEvent((event) => {
      // 拖出自家附件时不接管（那是往外拖，不是往里放）。
      if (attach.draggingOut.value) {
        if (event.payload.type === "drop") attach.draggingOut.value = false;
        dropTargetQ.value = null;
        return;
      }
      if (event.payload.type === "over") {
        const pos = event.payload.position;
        // 纵向布局下高亮即将落入的那张卡，松手前就能看出附件会进哪一题。
        dropTargetQ.value = verticalMode.value
          ? questionAtPoint(pos?.x ?? 0, pos?.y ?? 0)
          : null;
        return;
      }
      const wasOver = dropTargetQ.value;
      dropTargetQ.value = null;
      if (event.payload.type !== "drop") return;
      const pos = event.payload.position;
      const qIndex = wasOver ?? questionAtPoint(pos?.x ?? 0, pos?.y ?? 0);
      addDroppedPaths(event.payload.paths, qIndex);
    });
    // 设置变更实时生效（同进程内设置窗口保存后广播 general 配置）。
    unlistenSettings = await listen<{
      theme?: ThemeMode;
      language?: string;
      speechLanguage?: string;
      speechShortcut?: string;
      popupSubmitKey?: "cmdEnter" | "enter";
    }>("settings-updated", (e) => {
      // daemon 架构下由 Daemon 经 IPC 下发（独立 GUI Helper 进程）；单进程下由设置窗口同进程广播。
      if (typeof e.payload.theme === "string") {
        applyTheme(e.payload.theme);
      }
      if (typeof e.payload.language === "string") applyLanguage(e.payload.language);
      if (typeof e.payload.speechLanguage === "string")
        speech.speechLang.value = e.payload.speechLanguage || "auto";
      if (typeof e.payload.speechShortcut === "string")
        speech.speechShortcut.value = e.payload.speechShortcut;
      if (
        e.payload.popupSubmitKey === "enter" ||
        e.payload.popupSubmitKey === "cmdEnter"
      ) {
        popupSubmitKey.value = e.payload.popupSubmitKey;
      }
    });
    // 探测语音是否可用（macOS 26+）+ 订阅 speech-* 事件。
    await speech.initSpeech();
    // 版本自更新：拉初值 + 订阅实时变更。
    await update.initUpdateState();
    // 原生关闭按钮：后端阻止关闭并转发此事件 → 与 ⌘W 一致走二次确认。
    unlistenCloseReq = await listen("popup-close-requested", () => {
      if (isConfirm.value) requestConfirmClose();
      else requestCancel();
    });
    // 托盘「待答」子菜单点击本弹窗：后端已聚焦窗口，这里播放边框闪烁。
    unlistenFlash = await listen("popup-flash", () => {
      triggerFlash();
    });
    // 调用方 agent 信息（家族 + pid）由 daemon 从 caller_pid **异步** walk 得到（方案5/b），经
    // `agent-resolved` 后推：先 pull 初值（规避事件早于监听的竞态），再监听实时升级。拿到 pid 才把 badge
    // 升级成「可点 + ↗」（终端类型探测仍要跑进程链 ps，故也在此渲染后异步进行）。旧 daemon 可能随
    // popup_init 直接带 pid → 一并处理。
    if (init.agentPid != null) {
      void applyAgentResolved(init.agentKind, init.agentPid, undefined);
    }
    unlistenAgent = await listen<{
      kind?: string | null;
      pid?: number | null;
      launchId?: string | null;
    }>(
      "agent-resolved",
      (e) => {
        void applyAgentResolved(
          e.payload.kind,
          e.payload.pid,
          e.payload.launchId
        );
      },
    );
    try {
      const r = await popupAgentResolved();
      if (r.kind || r.pid != null || r.launchId) {
        void applyAgentResolved(r.kind, r.pid, r.launchId);
      }
    } catch {
      /* 无 daemon / 单进程回退：忽略 */
    }
    // R6 一次性引导：未配置任何 IM 渠道且未被关闭过时，页脚上方显示提示条。
    try {
      imTipVisible.value = await popupImTipVisible();
    } catch {
      /* 旧后端无此命令：忽略 */
    }
  }

  /// 应用 daemon 异步解析出的 agent 信息：补全家族 badge 文案，并据 pid 解析所在终端把 badge 升级成
  /// 「可点 + ↗」。幂等：pull 初值与事件可能各触发一次，重复设值无副作用。
  async function applyAgentResolved(
    kind: string | null | undefined,
    pid: number | null | undefined,
    launchId: string | null | undefined
  ) {
    if (kind && !agentKind.value) agentKind.value = kind;
    if (launchId) {
      agentLaunchId.value = launchId;
      if (isWindows) agentTerminal.value = "windows-terminal";
    }
    if (pid != null) {
      agentPid.value = pid;
      if (launchId && isWindows) return;
      try {
        agentTerminal.value = (await popupAgentTerminal(pid)) ?? null;
      } catch {
        /* 探测失败：保持纯文字 badge */
      }
    }
  }

  onBeforeUnmount(() => {
    window.removeEventListener("paste", onPaste);
    window.removeEventListener("keydown", onKeydown);
    window.removeEventListener("keyup", onKeyup);
    window.removeEventListener("blur", onWindowBlur);
    document.removeEventListener("mouseup", speech.onDocMouseUp);
    attach.disposeAttachments();
    unlistenDrop?.();
    unlistenSettings?.();
    update.disposeUpdateState();
    unlistenCloseReq?.();
    unlistenFlash?.();
    unlistenAgent?.();
    unlistenTodos?.();
    unlistenShow?.();
    if (timeTicker) window.clearInterval(timeTicker);
    if (flashTimer) window.clearTimeout(flashTimer);
    if (copiedTimer) window.clearTimeout(copiedTimer);
    io?.disconnect();
    io = null;
    composerResizeObserver?.disconnect();
    composerResizeObserver = null;
    scrollSpyPending = false;
    if (scrollRaf) cancelAnimationFrame(scrollRaf);
    speech.disposeSpeech();
  });

  return {
    // In-page find
    findActive: find.findActive,
    findQuery: find.findQuery,
    findCaseSensitive: find.findCaseSensitive,
    findCurrent: find.findCurrent,
    findTotal: find.findTotal,
    findCountLabel: find.findCountLabel,
    findNoMatch: find.findNoMatch,
    findInputEl: find.findInputEl,
    openFind: find.openFind,
    closeFind: find.closeFind,
    goFind: find.goFind,
    onFindQueryInput: find.onFindQueryInput,
    toggleFindCase: find.toggleFindCase,
    refreshFind: find.refreshFind,
    // 请求 / 加载态
    request,
    confirmRequest,
    isConfirm,
    loadError,
    // 视图 / 复制
    viewSource,
    copiedMessage,
    copyMessage,
    onContentClick,
    // 题目与作答
    questions,
    total,
    isMulti,
    verticalMode,
    selectOnly,
    single,
    current,
    actionQuestionIndex,
    currentQuestion,
    chosenByQ,
    inputByQ,
    imagesByQ,
    replyFilesByQ,
    chosen,
    userInput,
    images,
    replyFiles,
    transitionName,
    allViewed,
    expandedQ,
    optionHotkey,
    cardOptionHotkey,
    toggle,
    pickFiles,
    removeImage,
    removeReplyFile,
    autoGrow,
    onTextareaFocus,
    onTextareaBlur,
    onComposerInput,
    onComposerMouseDown,
    activateComposer,
    onComposerCompositionStart,
    onComposerCompositionEnd,
    focusQuestionAction,
    // DOM refs
    setInputRef,
    setComposerAnchorRef,
    setComposerHomeRef,
    setComposerDockRef,
    composerAnchorStyle,
    setCardRef,
    setSentinelRef,
    setThumbsRef,
    fileRef,
    contentRef,
    qHeaderRef,
    onFileChange,
    composerOwnerQ,
    dockedComposerQ,
    returnComposerHome,
    // Message / 头部
    messageText,
    showDescription,
    showQuestionHeader,
    questionHeaderLabel,
    headerPrefix,
    headerSuffix,
    agentLabel,
    agentFocusable,
    agentInline,
    onFocusAgentTerminal,
    projectName,
    projectPath,
    onOpenWorkspace,
    popupTimeRel,
    popupTimeAbs,
    attachments,
    // 导航 / 提交
    scrolled,
    cmdHeld,
    flashing,
    onScroll,
    onContentWheel,
    onDrop,
    dropTargetQ,
    setActive,
    goPrev,
    goNext,
    onQuestionEntered,
    canGoPrev,
    onLastQuestion,
    submitShowsCmdEnter,
    submitKeyLabel,
    submitPrimary,
    nextPrimary,
    lastSeen,
    canSubmit,
    submitting,
    submit,
    requestCancel,
    doCancel,
    showCancelConfirm,
    dismissCancelConfirm,
    // 确认弹窗（agent 权限确认）
    confirmChoiceIndex,
    confirmComment,
    confirmInput,
    showConfirmInput,
    confirmCanSubmit,
    confirmDetailHtml,
    confirmToolName,
    confirmRows,
    confirmVariantLevel,
    confirmVariantLevels,
    confirmVariantRecommendedLevel,
    selectConfirmVariantLevel,
    permissionEdit,
    permissionDiff,
    permissionDiffLoading,
    selectConfirmChoice,
    submitConfirm,
    requestConfirmClose,
    showConfirmCloseWarning,
    dismissConfirmCloseWarning,
    confirmCloseAndDeny,
    // 顶栏动作
    pinned,
    togglePin,
    agentConsoleAvailable,
    openAgentConsoleWindow,
    openSettingsWindow,
    openTodosWindow,
    openHistoryWindow,
    // R6 引导
    imTipVisible,
    imTipConfigure,
    imTipDismiss,
    // 待办下拉区（spec todo-whats-next D7）
    todos,
    todosOpen,
    todoChosenIds,
    todoChipsEnabled,
    todoSectionVisible,
    toggleTodo,
    removeTodo,
    // 子域
    ...speech,
    ...attach,
    ...update,
  };
}
