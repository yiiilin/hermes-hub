const CACHE_NAME = "hermes-hub-pwa-v4";
const EXAMPLE_ENTRY_PATH = "/examples/hermes-hub/index.html";
const EXAMPLE_ASSET_MANIFEST = Array.isArray(globalThis.EXAMPLE_ASSET_MANIFEST)
  ? globalThis.EXAMPLE_ASSET_MANIFEST.filter((item) => typeof item === "string")
  : [];
const HAS_EXAMPLE_APP_SHELL = EXAMPLE_ASSET_MANIFEST.length > 0;
const BASE_APP_SHELL = [
  "/",
  "/index.html",
  "/manifest.webmanifest",
  "/icons/icon.svg",
  "/icons/icon-192.png",
  "/icons/icon-512.png",
  "/icons/apple-touch-icon.png",
];
const EXAMPLE_APP_SHELL = HAS_EXAMPLE_APP_SHELL
  ? [EXAMPLE_ENTRY_PATH, ...EXAMPLE_ASSET_MANIFEST]
  : [];
// 正式包默认只缓存主应用；显式构建 example demo 时再追加它自己的 html 与哈希资源。
const APP_SHELL = Array.from(new Set([...BASE_APP_SHELL, ...EXAMPLE_APP_SHELL]));
const STATIC_PATH_PREFIXES = ["/assets/", "/icons/"];

self.addEventListener("install", (event) => {
  // 预缓存安装应用所需的最小资源，确保离线打开时仍有应用外壳可用。
  event.waitUntil(
    caches
      .open(CACHE_NAME)
      .then((cache) => cache.addAll(APP_SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener("activate", (event) => {
  // 清理旧版本缓存，避免用户安装后长期拿到过期资源。
  event.waitUntil(
    caches
      .keys()
      .then((cacheNames) =>
        Promise.all(
          cacheNames
            .filter((cacheName) => cacheName !== CACHE_NAME)
            .map((cacheName) => caches.delete(cacheName)),
        ),
      )
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const { request } = event;

  if (request.method !== "GET") {
    return;
  }

  const url = new URL(request.url);

  if (url.origin !== self.location.origin || shouldBypassRequest(url)) {
    return;
  }

  if (request.mode === "navigate") {
    event.respondWith(networkFirstNavigation(request));
    return;
  }

  if (isStaticAsset(url)) {
    event.respondWith(staleWhileRevalidate(request));
  }
});

function shouldBypassRequest(url) {
  // API 和内部接口依赖实时鉴权与服务端状态，不能被前端缓存拦截。
  return url.pathname.startsWith("/api") || url.pathname.startsWith("/internal");
}

function isStaticAsset(url) {
  return (
    APP_SHELL.includes(url.pathname) ||
    STATIC_PATH_PREFIXES.some((prefix) => url.pathname.startsWith(prefix))
  );
}

async function networkFirstNavigation(request) {
  const fallbackPath = navigationFallbackPath(new URL(request.url));

  try {
    const response = await fetch(request);

    if (response.ok && fallbackPath) {
      const cache = await caches.open(CACHE_NAME);
      await cache.put(fallbackPath, response.clone());
    }

    return response;
  } catch {
    if (!fallbackPath) {
      return Response.error();
    }
    // 多页构建下按导航前缀选择缓存入口；未打包 example 时不要把 /examples 误回退到主应用首页。
    return (await caches.match(fallbackPath)) || (await caches.match("/index.html")) || Response.error();
  }
}

function navigationFallbackPath(url) {
  if (url.pathname.startsWith("/examples/hermes-hub")) {
    return HAS_EXAMPLE_APP_SHELL ? EXAMPLE_ENTRY_PATH : null;
  }
  return "/index.html";
}

async function staleWhileRevalidate(request) {
  const cache = await caches.open(CACHE_NAME);
  const cachedResponse = await cache.match(request);
  const networkResponsePromise = fetch(request).then((response) => {
    if (response.ok) {
      void cache.put(request, response.clone());
    }

    return response;
  });

  return cachedResponse || networkResponsePromise;
}
