export interface ComposerDockGeometry {
  homeTop: number;
  homeBottom: number;
  dockedHomeHeight: number;
  viewportTop: number;
  viewportBottom: number;
  viewportBottomAfterUndock: number;
}

export interface ComposerDockThresholds {
  dockGap: number;
  returnGap: number;
}

export const DEFAULT_COMPOSER_DOCK_THRESHOLDS: ComposerDockThresholds = {
  dockGap: 0,
  returnGap: 10,
};

export const DEFAULT_DOCKED_TEXTAREA_MAX_HEIGHT = 120;

/**
 * Project the input-wrap height after only the textarea is capped in the dock.
 * This preserves fixed rows such as the multi-question composer action bar.
 */
export function projectedDockedComposerHomeHeight(
  inlineHomeHeight: number,
  inlineTextareaHeight: number,
  dockedTextareaMaxHeight = DEFAULT_DOCKED_TEXTAREA_MAX_HEIGHT
): number {
  if (inlineHomeHeight <= 0) return 0;
  if (inlineTextareaHeight <= 0 || dockedTextareaMaxHeight <= 0) {
    return inlineHomeHeight;
  }
  const textareaReduction = Math.max(
    0,
    inlineTextareaHeight - dockedTextareaMaxHeight
  );
  return Math.max(0, inlineHomeHeight - textareaReduction);
}

/** A focused editor owns user actions even when passive scrolling changes the viewport card. */
export function resolveActionQuestionIndex(
  viewportQuestion: number,
  focusedQuestion: number | null
): number {
  return focusedQuestion ?? viewportQuestion;
}

export function cmdEnterQuestionIndex(
  currentQuestion: number,
  focusedQuestion: number | null
): number {
  return resolveActionQuestionIndex(currentQuestion, focusedQuestion);
}

export function shouldRevealQuestionBeforeCmdEnter(
  questionIndex: number,
  focusedQuestion: number | null,
  dockedQuestion: number | null,
  cardOffScreen: boolean
): boolean {
  const focusedEditorIsDocked =
    focusedQuestion === questionIndex && dockedQuestion === questionIndex;
  return cardOffScreen && !focusedEditorIsDocked;
}

/** Only a real scroll event may hand the current-question pointer back to scroll-spy. */
export function shouldApplyScrollSpy(
  scrollEventPending: boolean,
  verticalMode: boolean,
  nowMs: number,
  activeLockUntilMs: number,
): boolean {
  return scrollEventPending && verticalMode && nowMs >= activeLockUntilMs;
}

export function shouldDeactivateOffscreenComposer(
  focusedQuestion: number | null,
  dockedQuestion: number | null,
  cardOffScreen: boolean
): boolean {
  return (
    focusedQuestion !== null &&
    dockedQuestion !== focusedQuestion &&
    cardOffScreen
  );
}

export function canComposerDock(
  focused: boolean,
  manuallyActivated: boolean,
  seenFullyInline: boolean,
  scrolledUpAfterActivation: boolean
): boolean {
  return (
    focused &&
    scrolledUpAfterActivation &&
    (manuallyActivated || seenFullyInline)
  );
}

export function composerHomeVisibleRatio(geometry: ComposerDockGeometry): number {
  const height = geometry.homeBottom - geometry.homeTop;
  if (height <= 0) return 0;
  const visibleHeight = Math.max(
    0,
    Math.min(geometry.homeBottom, geometry.viewportBottom) -
      Math.max(geometry.homeTop, geometry.viewportTop)
  );
  return Math.min(1, visibleHeight / height);
}

export function isComposerHomeFullyVisible(
  geometry: ComposerDockGeometry
): boolean {
  return (
    geometry.homeTop >= geometry.viewportTop &&
    geometry.homeBottom <= geometry.viewportBottom
  );
}

export function resolveComposerDocked(
  currentlyDocked: boolean,
  ownerCanDock: boolean,
  geometry: ComposerDockGeometry,
  thresholds: ComposerDockThresholds = DEFAULT_COMPOSER_DOCK_THRESHOLDS
): boolean {
  if (!ownerCanDock) return false;

  // Bottom docking only covers looking back at content above the composer. If the source has
  // moved above the viewport, keep the editor in its normal document position.
  if (geometry.homeTop < geometry.viewportTop) return false;

  // A tall inline editor is shorter in the dock. Let the viewport clip it progressively, then
  // move it only when the portion that will remain visible reaches the bottom edge. The same
  // projected edge is used for returning home, so 240px <-> 120px never moves the top abruptly.
  const projectedDockedHomeBottom =
    geometry.homeTop + geometry.dockedHomeHeight;

  if (!currentlyDocked) {
    return (
      projectedDockedHomeBottom >
      geometry.viewportBottom - thresholds.dockGap
    );
  }

  const returnedInsideViewport =
    projectedDockedHomeBottom <=
    geometry.viewportBottomAfterUndock - thresholds.returnGap;
  return !returnedInsideViewport;
}
