import { afterEach, describe, expect, it, vi } from "vitest";

import { createExampleAuthFlow } from "./auth-flow";
import type { ExampleHubConfig } from "./hub-client";
import {
  EXAMPLE_STORAGE_KEYS,
  loadStoredAccessToken,
  saveStoredConfig,
  saveStoredOAuthState,
} from "./storage";

const TEST_CONFIG: ExampleHubConfig = {
  baseUrl: "https://hub.example",
  clientId: "client-demo",
  clientSecret: "secret-demo",
  redirectUri: "https://app.example/examples/hermes-hub/",
  scopes: "openid profile email",
};

describe("auth flow", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
    window.history.pushState({}, "", "/");
  });

  it("生成 OAuth 授权地址并记录防重放 state", () => {
    const client = {
      exchangeAuthorizationCode: vi.fn(),
      getUserInfo: vi.fn(),
    };
    const navigate = vi.fn();
    const flow = createExampleAuthFlow(client, { navigate });

    saveStoredConfig(TEST_CONFIG);
    flow.beginAuthorization(TEST_CONFIG);

    const url = new URL(navigate.mock.calls[0]![0] as string);
    expect(url.origin).toBe("https://hub.example");
    expect(url.pathname).toBe("/api/oauth/authorize");
    expect(url.searchParams.get("response_type")).toBe("code");
    expect(url.searchParams.get("client_id")).toBe("client-demo");
    expect(url.searchParams.get("redirect_uri")).toBe(TEST_CONFIG.redirectUri);
    expect(url.searchParams.get("scope")).toBe("openid profile email");
    expect(url.searchParams.get("state")).toBeTruthy();
    expect(localStorage.getItem(EXAMPLE_STORAGE_KEYS.oauthState)).toBe(
      url.searchParams.get("state"),
    );
  });

  it("回调成功后交换 token、拉取 userinfo 并清理 URL", async () => {
    const client = {
      exchangeAuthorizationCode: vi.fn().mockResolvedValue({
        access_token: "oauth-token",
        token_type: "Bearer",
        expires_in: 604800,
        scope: "openid profile email",
      }),
      getUserInfo: vi.fn().mockResolvedValue({
        id: "user-1",
        sub: "user-1",
        email: "demo@example.com",
        integration_id: "crm",
        toolset_names: ["save_note", "search_notes"],
      }),
    };
    const replaceStateSpy = vi.spyOn(window.history, "replaceState");
    const flow = createExampleAuthFlow(client);

    saveStoredConfig(TEST_CONFIG);
    saveStoredOAuthState("state-1");
    window.history.pushState(
      {},
      "",
      "/examples/hermes-hub/?code=auth-code-1&state=state-1",
    );

    const result = await flow.completeAuthorizationFromLocation(TEST_CONFIG);

    expect(client.exchangeAuthorizationCode).toHaveBeenCalledWith(TEST_CONFIG, "auth-code-1");
    expect(client.getUserInfo).toHaveBeenCalledWith(TEST_CONFIG, "oauth-token");
    expect(loadStoredAccessToken()).toMatchObject({
      accessToken: "oauth-token",
      scope: "openid profile email",
    });
    expect(result).toMatchObject({
      accessToken: {
        accessToken: "oauth-token",
      },
      userInfo: {
        email: "demo@example.com",
      },
    });
    expect(localStorage.getItem(EXAMPLE_STORAGE_KEYS.oauthState)).toBeNull();
    expect(replaceStateSpy).toHaveBeenCalledWith(
      {},
      "",
      "/examples/hermes-hub/",
    );
  });

  it("回调失败时会清理 URL 和本地 oauth state，避免刷新后重复消费失效 code", async () => {
    const client = {
      exchangeAuthorizationCode: vi.fn().mockRejectedValue(new Error("invalid authorization code")),
      getUserInfo: vi.fn(),
    };
    const replaceStateSpy = vi.spyOn(window.history, "replaceState");
    const flow = createExampleAuthFlow(client);

    saveStoredConfig(TEST_CONFIG);
    saveStoredOAuthState("state-1");
    window.history.pushState(
      {},
      "",
      "/examples/hermes-hub/?code=expired-code&state=state-1",
    );

    await expect(flow.completeAuthorizationFromLocation(TEST_CONFIG)).rejects.toThrow(
      "invalid authorization code",
    );
    expect(localStorage.getItem(EXAMPLE_STORAGE_KEYS.oauthState)).toBeNull();
    expect(replaceStateSpy).toHaveBeenCalledWith(
      {},
      "",
      "/examples/hermes-hub/",
    );
  });

  it("同一个 OAuth 回调在并发恢复时只会消费一次授权码", async () => {
    const tokenDeferred = createDeferred<{
      access_token: string;
      token_type: string;
      expires_in: number;
      scope: string;
    }>();
    const client = {
      exchangeAuthorizationCode: vi.fn().mockImplementation(
        () => tokenDeferred.promise,
      ),
      getUserInfo: vi.fn().mockResolvedValue({
        id: "user-1",
        sub: "user-1",
        email: "demo@example.com",
        integration_id: "crm",
        toolset_names: ["save_note", "search_notes"],
      }),
    };
    const flow = createExampleAuthFlow(client);

    saveStoredConfig(TEST_CONFIG);
    saveStoredOAuthState("state-1");
    window.history.pushState(
      {},
      "",
      "/examples/hermes-hub/?code=one-time-code&state=state-1",
    );

    const firstCall = flow.completeAuthorizationFromLocation(TEST_CONFIG);
    const secondCall = flow.completeAuthorizationFromLocation(TEST_CONFIG);
    tokenDeferred.resolve({
      access_token: "oauth-token",
      token_type: "Bearer",
      expires_in: 604800,
      scope: "openid profile email",
    });

    const [firstResult, secondResult] = await Promise.all([firstCall, secondCall]);
    expect(client.exchangeAuthorizationCode).toHaveBeenCalledTimes(1);
    expect(firstResult).toMatchObject({
      accessToken: {
        accessToken: "oauth-token",
      },
    });
    expect(secondResult).toMatchObject({
      accessToken: {
        accessToken: "oauth-token",
      },
    });
  });
});

function createDeferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}
