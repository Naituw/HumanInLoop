export const MERMAID_MAX_DIAGRAMS = 10;
export const MERMAID_MAX_TEXT_SIZE = 40_000;
export const MERMAID_MAX_EDGES = 400;
export const MERMAID_MIN_FITTED_FONT_SIZE = 12;

/** Fit toward the available width without shrinking visible labels below 12px. */
export function mermaidFitScale(
  intrinsicWidth: number,
  availableWidth: number,
  sourceFontSize: number,
): number {
  if (
    !Number.isFinite(intrinsicWidth) ||
    !Number.isFinite(availableWidth) ||
    intrinsicWidth <= 0 ||
    availableWidth <= 0
  ) {
    return 1;
  }
  const fontSize = Number.isFinite(sourceFontSize)
    ? Math.min(24, Math.max(10, sourceFontSize))
    : 14;
  const minimumScale = Math.min(1, MERMAID_MIN_FITTED_FONT_SIZE / fontSize);
  return Math.min(1, Math.max(minimumScale, availableWidth / intrinsicWidth));
}
