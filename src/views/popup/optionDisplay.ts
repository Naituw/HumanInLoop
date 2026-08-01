import type { OptionItem } from "../../lib/types";

/** Keep the stored option value intact while hiding the legacy todo display prefix. */
export function optionDisplayText(
  option: OptionItem,
  whatsNext: boolean,
  todoPrefix: string,
): string {
  if (!whatsNext || !option.todoId || !option.text.startsWith(todoPrefix)) {
    return option.text;
  }
  return option.todoText ?? option.text.slice(todoPrefix.length);
}
