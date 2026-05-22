const SERVICE_WORKER_URL = "/service-worker.js";

export function registerServiceWorker() {
  const viteMeta = import.meta as unknown as {
    readonly env?: { readonly PROD?: boolean };
  };

  // 只在浏览器支持且生产构建中注册，避免开发环境缓存 Vite 模块导致调试混乱。
  if (!("serviceWorker" in navigator) || viteMeta.env?.PROD !== true) {
    return;
  }

  window.addEventListener("load", () => {
    // 等首屏资源加载完成后再注册，降低 Service Worker 对首屏渲染的影响。
    void navigator.serviceWorker
      .register(SERVICE_WORKER_URL, { scope: "/" })
      .catch((error: unknown) => {
        console.warn("Service Worker 注册失败", error);
      });
  });
}
