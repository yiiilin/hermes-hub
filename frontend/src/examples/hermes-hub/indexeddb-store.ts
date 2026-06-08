export type ExampleNote = {
  id: string;
  title?: string;
  content: string;
  tags: string[];
  createdAt: number;
  updatedAt: number;
};

export type ExampleNoteStore = {
  saveNote: (input: {
    title?: string;
    content: string;
    tags?: string[];
  }) => Promise<ExampleNote>;
  searchNotes: (query: string, limit?: number) => Promise<ExampleNote[]>;
  listNotes: () => Promise<ExampleNote[]>;
};

const DATABASE_NAME = "hermes-hub-example-notes";
const DATABASE_VERSION = 1;
const NOTE_STORE_NAME = "notes";

function createNoteId(): string {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return `note-${crypto.randomUUID()}`;
  }
  return `note-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function normalizeTags(tags: string[] | undefined): string[] {
  return (tags ?? [])
    .map((tag) => tag.trim())
    .filter((tag) => tag.length > 0);
}

function openDatabase(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open(DATABASE_NAME, DATABASE_VERSION);
    request.onupgradeneeded = () => {
      const database = request.result;
      if (!database.objectStoreNames.contains(NOTE_STORE_NAME)) {
        // 示例只需要一个简单的笔记表，方便把本地工具行为保持在浏览器里。
        database.createObjectStore(NOTE_STORE_NAME, { keyPath: "id" });
      }
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("无法打开 IndexedDB"));
  });
}

function runObjectStoreRequest<T>(request: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error ?? new Error("IndexedDB 请求失败"));
  });
}

export function createIndexedDbNoteStore(): ExampleNoteStore {
  return {
    async saveNote(input) {
      const database = await openDatabase();
      const now = Date.now();
      const note: ExampleNote = {
        id: createNoteId(),
        title: input.title?.trim() || undefined,
        content: input.content.trim(),
        tags: normalizeTags(input.tags),
        createdAt: now,
        updatedAt: now,
      };
      const transaction = database.transaction(NOTE_STORE_NAME, "readwrite");
      const store = transaction.objectStore(NOTE_STORE_NAME);
      await runObjectStoreRequest(store.put(note));
      database.close();
      return note;
    },
    async searchNotes(query, limit = 5) {
      const allNotes = await this.listNotes();
      const normalizedQuery = query.trim().toLowerCase();
      if (!normalizedQuery) {
        return allNotes.slice(0, limit);
      }
      return allNotes
        .filter((note) => {
          const haystacks = [note.title ?? "", note.content, note.tags.join(" ")];
          return haystacks.some((value) => value.toLowerCase().includes(normalizedQuery));
        })
        .slice(0, Math.max(limit, 1));
    },
    async listNotes() {
      const database = await openDatabase();
      const transaction = database.transaction(NOTE_STORE_NAME, "readonly");
      const store = transaction.objectStore(NOTE_STORE_NAME);
      const notes = await runObjectStoreRequest(store.getAll());
      database.close();
      return [...notes].sort((left, right) => right.updatedAt - left.updatedAt);
    },
  };
}
