import fs from "node:fs";
import path from "node:path";
import vm from "node:vm";

import { describe, expect, it } from "vitest";

describe("service worker navigation fallback", () => {
  it("正式包默认不预缓存独立 example 入口", async () => {
    const { listeners, cacheStore } = loadServiceWorker();
    const waitUntilPromises: Promise<unknown>[] = [];

    listeners.install?.({
      waitUntil(promise: Promise<unknown>) {
        waitUntilPromises.push(promise);
      },
    });
    await Promise.all(waitUntilPromises);

    expect(Array.from(cacheStore.keys())).not.toContain("/examples/hermes-hub/index.html");
  });

  it("显式构建 example demo 时预缓存包含它的 html 入口和静态资源", async () => {
    const { listeners, cacheStore } = loadServiceWorker({
      manifestAssets: [
        "/assets/hermesHubExample-entry.js",
        "/assets/hermesHubExample-entry.css",
      ],
    });
    const waitUntilPromises: Promise<unknown>[] = [];

    listeners.install?.({
      waitUntil(promise: Promise<unknown>) {
        waitUntilPromises.push(promise);
      },
    });
    await Promise.all(waitUntilPromises);

    expect(Array.from(cacheStore.keys())).toContain("/examples/hermes-hub/index.html");
    expect(Array.from(cacheStore.keys())).toContain("/assets/hermesHubExample-entry.js");
    expect(Array.from(cacheStore.keys())).toContain("/assets/hermesHubExample-entry.css");
  });

  it("example 导航离线时回退到自己的 html 入口，而不是主应用首页", async () => {
    const { listeners, cacheStore } = loadServiceWorker({
      manifestAssets: ["/assets/hermesHubExample-entry.js"],
      fetchImpl: async () => {
        throw new Error("offline");
      },
    });
    cacheStore.set(
      "/examples/hermes-hub/index.html",
      new Response("<html>example shell</html>", { status: 200 }),
    );
    cacheStore.set(
      "/index.html",
      new Response("<html>main shell</html>", { status: 200 }),
    );

    let responsePromise: Promise<Response> | null = null;
    listeners.fetch?.({
      request: {
        method: "GET",
        mode: "navigate",
        url: "https://hub.example/examples/hermes-hub/",
      },
      respondWith(promise: Promise<Response>) {
        responsePromise = promise;
      },
    });

    expect(responsePromise).not.toBeNull();
    const response = await responsePromise!;
    expect(await response.text()).toContain("example shell");
  });

  it("未打包 example 时，/examples 离线导航不能误回退到主应用首页", async () => {
    const { listeners, cacheStore } = loadServiceWorker({
      fetchImpl: async () => {
        throw new Error("offline");
      },
    });
    cacheStore.set(
      "/index.html",
      new Response("<html>main shell</html>", { status: 200 }),
    );

    let responsePromise: Promise<Response> | null = null;
    listeners.fetch?.({
      request: {
        method: "GET",
        mode: "navigate",
        url: "https://hub.example/examples/hermes-hub/",
      },
      respondWith(promise: Promise<Response>) {
        responsePromise = promise;
      },
    });

    expect(responsePromise).not.toBeNull();
    const response = await responsePromise!;
    expect(response.type).toBe("error");
  });
});

function loadServiceWorker(options: { fetchImpl?: typeof fetch; manifestAssets?: string[] } = {}) {
  const listeners: Record<string, (event: any) => void> = {};
  const cacheStore = new Map<string, Response>();

  const cachesApi = {
    async open() {
      return {
        async addAll(paths: string[]) {
          for (const item of paths) {
            cacheStore.set(item, new Response(item, { status: 200 }));
          }
        },
        async put(key: string, response: Response) {
          cacheStore.set(key, response);
        },
        async match(key: string) {
          return cacheStore.get(key) ?? undefined;
        },
      };
    },
    async match(key: string) {
      return cacheStore.get(key) ?? undefined;
    },
    async keys() {
      return ["hermes-hub-pwa-v2"];
    },
    async delete() {
      return true;
    },
  };

  const context = vm.createContext({
    URL,
    Response,
    caches: cachesApi,
    fetch: options.fetchImpl ?? fetch,
    console,
    EXAMPLE_ASSET_MANIFEST: options.manifestAssets ?? [],
    self: {
      location: {
        origin: "https://hub.example",
      },
      skipWaiting: async () => undefined,
      clients: {
        claim: async () => undefined,
      },
      addEventListener(type: string, listener: (event: any) => void) {
        listeners[type] = listener;
      },
    },
  });

  const source = fs.readFileSync(
    path.resolve("/usr/local/src/project/hermes-hub/frontend/public/service-worker.js"),
    "utf8",
  );
  vm.runInContext(source, context, {
    filename: "service-worker.js",
  });

  return { listeners, cacheStore };
}
