import { afterEach, beforeEach, describe, expect, it } from "vitest";

import { createIndexedDbNoteStore } from "./indexeddb-store";

describe("indexeddb note store", () => {
  beforeEach(() => {
    installFakeIndexedDb();
  });

  afterEach(() => {
    teardownFakeIndexedDb();
  });

  it("save_note 会写入本地笔记并带上时间信息", async () => {
    const store = createIndexedDbNoteStore();

    const saved = await store.saveNote({
      title: "会议纪要",
      content: "记录今天和 Hub 对接的联调结果",
      tags: ["hub", "demo"],
    });

    expect(saved.id).toBeTruthy();
    expect(saved.createdAt).toBeGreaterThan(0);
    expect(saved.updatedAt).toBe(saved.createdAt);

    const notes = await store.listNotes();
    expect(notes).toHaveLength(1);
    expect(notes[0]).toMatchObject({
      title: "会议纪要",
      content: "记录今天和 Hub 对接的联调结果",
      tags: ["hub", "demo"],
    });
  });

  it("search_notes 能命中标题、正文和标签，并在重新打开后保留数据", async () => {
    const firstStore = createIndexedDbNoteStore();
    await firstStore.saveNote({
      title: "CRM 审批",
      content: "客户要求保存一条关于审批状态的笔记",
      tags: ["crm", "审批"],
    });
    await firstStore.saveNote({
      title: "购物清单",
      content: "牛奶和咖啡",
      tags: ["生活"],
    });

    const secondStore = createIndexedDbNoteStore();
    const byTitle = await secondStore.searchNotes("CRM", 10);
    const byContent = await secondStore.searchNotes("咖啡", 10);
    const byTag = await secondStore.searchNotes("审批", 10);

    expect(byTitle).toHaveLength(1);
    expect(byTitle[0]?.title).toBe("CRM 审批");
    expect(byContent).toHaveLength(1);
    expect(byContent[0]?.title).toBe("购物清单");
    expect(byTag).toHaveLength(1);
    expect(byTag[0]?.tags).toContain("审批");
  });
});

type FakeRecord = Record<string, unknown> & { id: string };

type FakeDatabaseState = {
  stores: Map<string, Map<string, FakeRecord>>;
};

let fakeDatabases = new Map<string, FakeDatabaseState>();
let originalIndexedDb: IDBFactory | undefined;

function installFakeIndexedDb() {
  originalIndexedDb = globalThis.indexedDB;
  fakeDatabases = new Map();
  Object.defineProperty(globalThis, "indexedDB", {
    configurable: true,
    value: {
      open(name: string) {
        const request = createRequest<IDBDatabase>();
        queueMicrotask(() => {
          let state = fakeDatabases.get(name);
          const isNewDatabase = state === undefined;
          if (!state) {
            state = {
              stores: new Map(),
            };
            fakeDatabases.set(name, state);
          }
          const database = createDatabase(state);
          request.result = database;
          if (isNewDatabase) {
            request.onupgradeneeded?.({ target: request } as unknown as IDBVersionChangeEvent);
          }
          request.onsuccess?.({ target: request } as unknown as Event);
        });
        return request;
      },
    } as IDBFactory,
  });
}

function teardownFakeIndexedDb() {
  Object.defineProperty(globalThis, "indexedDB", {
    configurable: true,
    value: originalIndexedDb,
  });
}

function createDatabase(state: FakeDatabaseState): IDBDatabase {
  const database = {
    objectStoreNames: {
      contains(name: string) {
        return state.stores.has(name);
      },
    },
    createObjectStore(name: string) {
      if (!state.stores.has(name)) {
        state.stores.set(name, new Map());
      }
      return createObjectStore(state.stores.get(name)!);
    },
    transaction(name: string) {
      if (!state.stores.has(name)) {
        state.stores.set(name, new Map());
      }
      return {
        objectStore() {
          return createObjectStore(state.stores.get(name)!);
        },
      } as unknown as IDBTransaction;
    },
    close() {
      return undefined;
    },
  };

  return database as unknown as IDBDatabase;
}

function createObjectStore(store: Map<string, FakeRecord>): IDBObjectStore {
  return {
    put(value: FakeRecord) {
      const request = createRequest<IDBValidKey>();
      queueMicrotask(() => {
        store.set(String(value.id), structuredClone(value));
        request.result = value.id;
        request.onsuccess?.({ target: request } as unknown as Event);
      });
      return request;
    },
    getAll() {
      const request = createRequest<FakeRecord[]>();
      queueMicrotask(() => {
        request.result = Array.from(store.values()).map((value) => structuredClone(value));
        request.onsuccess?.({ target: request } as unknown as Event);
      });
      return request;
    },
  } as unknown as IDBObjectStore;
}

function createRequest<T>() {
  return {
    result: undefined as T | undefined,
    error: null,
    onsuccess: null as ((event: Event) => void) | null,
    onerror: null as ((event: Event) => void) | null,
    onupgradeneeded: null as ((event: IDBVersionChangeEvent) => void) | null,
  };
}
