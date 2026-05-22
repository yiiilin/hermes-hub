import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { App } from "./app";
import { registerServiceWorker } from "./pwa";
import "./styles.css";

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <App />
  </StrictMode>,
);

registerServiceWorker();
