const CACHE_NAME = "hermes-hub-pwa-v1";
const APP_SHELL = [
  "/",
  "/index.html",
  "/manifest.webmanifest",
  "/icons/icon.svg",
  "/icons/icon-192.png",
  "/icons/icon-512.png",
  "/icons/apple-touch-icon.png",
];
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
  try {
    const response = await fetch(request);

    if (response.ok) {
      const cache = await caches.open(CACHE_NAME);
      await cache.put("/index.html", response.clone());
    }

    return response;
  } catch {
    // 离线导航回退到最近一次缓存的应用入口，让已安装应用可以打开。
    return (await caches.match("/index.html")) || Response.error();
  }
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
