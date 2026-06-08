import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { HermesHubExampleApp } from "./app";
import { registerServiceWorker } from "../../pwa";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <HermesHubExampleApp />
  </StrictMode>,
);

// 示例页是独立入口，也需要自行注册 Service Worker，才能让离线回退策略真正生效。
registerServiceWorker();
