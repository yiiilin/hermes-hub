import type { ExampleToolDefinition } from "./hub-client";
import type { ExampleNote, ExampleNoteStore } from "./indexeddb-store";

export type LocalToolExecutionResult = {
  resultText: string;
  summary: string;
};

export type LocalToolRegistry = {
  definitions: ExampleToolDefinition[];
  runTool: (
    toolName: string,
    argumentsObject: Record<string, unknown>,
  ) => Promise<LocalToolExecutionResult>;
};

function readString(value: unknown, fieldName: string, required = false): string | undefined {
  if (value === undefined || value === null || value === "") {
    if (required) {
      throw new Error(`缺少必填参数 ${fieldName}`);
    }
    return undefined;
  }
  if (typeof value !== "string") {
    throw new Error(`${fieldName} 必须是字符串`);
  }
  const normalized = value.trim();
  if (!normalized && required) {
    throw new Error(`缺少必填参数 ${fieldName}`);
  }
  return normalized || undefined;
}

function readStringArray(value: unknown, fieldName: string): string[] | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (!Array.isArray(value) || value.some((item) => typeof item !== "string")) {
    throw new Error(`${fieldName} 必须是字符串数组`);
  }
  return value.map((item) => item.trim()).filter((item) => item.length > 0);
}

function readPositiveInteger(value: unknown, fieldName: string): number | undefined {
  if (value === undefined || value === null) {
    return undefined;
  }
  if (typeof value !== "number" || !Number.isInteger(value) || value <= 0) {
    throw new Error(`${fieldName} 必须是正整数`);
  }
  return value;
}

function renderNoteSummary(note: ExampleNote): string {
  const title = note.title ? `《${note.title}》` : "未命名笔记";
  const tags = note.tags.length > 0 ? `，标签：${note.tags.join("、")}` : "";
  return `${title}，ID：${note.id}${tags}`;
}

export function createLocalToolRegistry(store: ExampleNoteStore): LocalToolRegistry {
  return {
    definitions: [
      {
        name: "save_note",
        description: "把当前对话里的内容保存到浏览器本地 IndexedDB 笔记库。",
        parameters: {
          type: "object",
          properties: {
            title: {
              type: "string",
              description: "可选标题",
            },
            content: {
              type: "string",
              description: "要保存的笔记正文",
            },
            tags: {
              type: "array",
              items: { type: "string" },
              description: "可选标签列表",
            },
          },
          required: ["content"],
        },
      },
      {
        name: "search_notes",
        description: "在浏览器本地笔记库里按标题、正文和标签做简单检索。",
        parameters: {
          type: "object",
          properties: {
            query: {
              type: "string",
              description: "检索关键词",
            },
            limit: {
              type: "integer",
              description: "返回条数上限，默认 5",
            },
          },
          required: ["query"],
        },
      },
    ],
    async runTool(toolName, argumentsObject) {
      if (toolName === "save_note") {
        const content = readString(argumentsObject.content, "content", true)!;
        const title = readString(argumentsObject.title, "title");
        const tags = readStringArray(argumentsObject.tags, "tags");
        const saved = await store.saveNote({ title, content, tags });
        return {
          resultText: `已保存笔记：${renderNoteSummary(saved)}`,
          summary: "已写入本地笔记库",
        };
      }

      if (toolName === "search_notes") {
        const query = readString(argumentsObject.query, "query", true)!;
        const limit = readPositiveInteger(argumentsObject.limit, "limit");
        const notes = await store.searchNotes(query, limit ?? 5);
        if (notes.length === 0) {
          return {
            resultText: `未找到与“${query}”匹配的本地笔记。`,
            summary: "未找到匹配笔记",
          };
        }
        const items = notes
          .map((note, index) => `${index + 1}. ${renderNoteSummary(note)}\n${note.content}`)
          .join("\n\n");
        return {
          resultText: `找到 ${notes.length} 条本地笔记：\n${items}`,
          summary: `找到 ${notes.length} 条笔记`,
        };
      }

      throw new Error(`未知工具：${toolName}`);
    },
  };
}
