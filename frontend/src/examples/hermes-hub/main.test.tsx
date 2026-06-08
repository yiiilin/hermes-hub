import { afterEach, describe, expect, it, vi } from "vitest";

const render = vi.fn();
const createRoot = vi.fn(() => ({
  render,
}));
const registerServiceWorker = vi.fn();

vi.mock("react-dom/client", () => ({
  createRoot,
}));

vi.mock("./app", () => ({
  HermesHubExampleApp: () => null,
}));

vi.mock("../../pwa", () => ({
  registerServiceWorker,
}));

describe("example main entry", () => {
  afterEach(() => {
    vi.clearAllMocks();
    vi.resetModules();
    document.body.innerHTML = "";
  });

  it("会在挂载示例页面时注册 service worker", async () => {
    document.body.innerHTML = '<div id="root"></div>';

    await import("./main");

    expect(createRoot).toHaveBeenCalledTimes(1);
    expect(registerServiceWorker).toHaveBeenCalledTimes(1);
    expect(render).toHaveBeenCalledTimes(1);
  });
});
