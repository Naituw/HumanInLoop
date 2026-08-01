export function nextTodoSelection(
  ids: string[],
  selectedId: string | null,
  direction: -1 | 1
): string | null {
  if (!ids.length) return null;
  const current = selectedId ? ids.indexOf(selectedId) : -1;
  if (current < 0) return direction > 0 ? ids[0] : ids[ids.length - 1];
  return ids[Math.max(0, Math.min(ids.length - 1, current + direction))];
}

export function clipboardImageFiles(items?: DataTransferItemList): File[] {
  if (!items) return [];
  const files: File[] = [];
  for (let index = 0; index < items.length; index++) {
    const item = items[index];
    if (item.kind !== "file" || !item.type.startsWith("image/")) continue;
    const file = item.getAsFile();
    if (file) files.push(file);
  }
  return files;
}
