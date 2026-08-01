import { describe, expect, it } from "vitest";
import type { OptionItem, TodoEntry } from "../../lib/types";
import {
  refreshWhatsNextTodos,
  selectedWhatsNextTodo,
} from "./whatsNextTodos";

const suggestion: OptionItem = {
  text: "Add tests",
  recommended: true,
};
const end: OptionItem = { text: "End this turn", recommended: false };

function todo(id: string, text = `task ${id}`): TodoEntry {
  return { id, text, createdAtMs: 1 };
}

describe("refreshWhatsNextTodos", () => {
  it("keeps suggestions and end while rebuilding the bounded TODO slice", () => {
    const latest = Array.from({ length: 10 }, (_, index) => todo(`${index}`));
    const refreshed = refreshWhatsNextTodos(
      [suggestion, end],
      [suggestion, end],
      latest,
      [],
      "Run todo: ",
      (count) => `【${count} attachments】`,
    );

    expect(refreshed.options).toHaveLength(10);
    expect(refreshed.options[0]).toEqual(suggestion);
    expect(refreshed.options[refreshed.options.length - 1]).toEqual(end);
    expect(refreshed.options[1]).toMatchObject({
      text: "Run todo: task 0",
      todoId: "0",
    });
    expect(refreshed.hiddenTodos).toBe(2);
  });

  it("uses the localized bracket badge for attachment counts", () => {
    const entry = todo("todo-1", "review brief");
    entry.attachments = [
      {
        id: "attachment-1",
        name: "brief.md",
        size: 1,
        isImage: false,
        sourcePath: "/tmp/brief.md",
        path: "/tmp/brief.md",
        storage: "reference",
        available: true,
      },
    ];
    const refreshed = refreshWhatsNextTodos(
      [end],
      [end],
      [entry],
      [],
      "Run todo: ",
      (count) => `【${count} attachments】`,
    );
    expect(refreshed.options[0]).toMatchObject({
      text: "Run todo: review brief 【1 attachments】",
      todoText: "review brief",
    });
    expect(refreshed.options[0].text).not.toContain("📎");
  });

  it("freezes a selected TODO snapshot across edits and clears removed selections", () => {
    const oldTodo: OptionItem = {
      text: "Run todo: old text",
      recommended: false,
      todoId: "todo-1",
    };
    const kept = refreshWhatsNextTodos(
      [suggestion, oldTodo, end],
      [suggestion, end],
      [todo("todo-1", "new text")],
      [oldTodo.text],
      "Run todo: ",
      (count) => `【${count} attachments】`,
    );
    expect(kept.selectedOptions).toEqual(["Run todo: old text"]);

    const removed = refreshWhatsNextTodos(
      kept.options,
      [suggestion, end],
      [],
      kept.selectedOptions,
      "Run todo: ",
      (count) => `【${count} attachments】`,
    );
    expect(removed.selectedOptions).toEqual([]);
  });

  it("preserves a static suggestion selection", () => {
    const refreshed = refreshWhatsNextTodos(
      [suggestion, end],
      [suggestion, end],
      [todo("todo-1")],
      [suggestion.text],
      "Run todo: ",
      (count) => `【${count} attachments】`,
    );
    expect(refreshed.selectedOptions).toEqual([suggestion.text]);
  });

  it("omits overflow noise when suggestions consume every task slot", () => {
    const suggestions = Array.from({ length: 9 }, (_, index) => ({
      text: `suggestion ${index}`,
      recommended: false,
    }));
    const refreshed = refreshWhatsNextTodos(
      [...suggestions, end],
      [...suggestions, end],
      [todo("todo-1")],
      [],
      "Run todo: ",
      (count) => `【${count} attachments】`,
    );
    expect(refreshed.options).toEqual([...suggestions, end]);
    expect(refreshed.hiddenTodos).toBe(0);
  });
});

describe("selectedWhatsNextTodo", () => {
  it("returns the frozen raw text, attachments, and stable id for submission", () => {
    const option: OptionItem = {
      text: "Run todo: old text 【1 attachment】",
      recommended: false,
      todoId: "todo-1",
      todoText: "old text",
      todoAttachments: [
        {
          id: "attachment-1",
          name: "brief.md",
          path: "/tmp/brief.md",
          sourcePath: "/source/brief.md",
          storage: "managed",
        },
      ],
    };
    expect(
      selectedWhatsNextTodo(
        [option, end],
        [option.text],
        [todo("todo-1", "new text")],
        "Run todo: ",
      ),
    ).toEqual({
      id: "todo-1",
      text: "old text",
      optionText: option.text,
      attachments: option.todoAttachments,
    });
  });
});
