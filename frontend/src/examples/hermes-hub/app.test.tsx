import { StrictMode } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { createExampleAuthFlow } from "./auth-flow";
import type { ExampleAuthFlow } from "./auth-flow";
import type {
  ExampleHubClient,
  ExampleHubConfig,
  ExampleSessionEvent,
  ExampleSessionSummary,
  ExampleUserInfo,
} from "./hub-client";
import type { ExampleNoteStore } from "./indexeddb-store";
import { HermesHubExampleApp } from "./app";
import { EXAMPLE_STORAGE_KEYS } from "./storage";

const TEST_CONFIG: ExampleHubConfig = {
  baseUrl: "https://hub.example",
  clientId: "client-demo",
  clientSecret: "secret-demo",
  redirectUri: "https://app.example/examples/hermes-hub/",
  scopes: "openid profile email",
};

const TEST_USER_INFO: ExampleUserInfo = {
  id: "user-1",
  sub: "user-1",
  email: "demo@example.com",
  integration_id: "crm",
  toolset_names: ["save_note", "search_notes"],
};

const TEST_SESSION: ExampleSessionSummary = {
  id: "session-1",
  title: "演示会话",
  isHome: false,
  deletable: true,
  hiddenFromWeb: true,
  createdAt: 1,
  updatedAt: 1,
};

describe("Hermes Hub example app", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    localStorage.clear();
  });

  it("能保存配置并同步本地工具定义", async () => {
    const client = createClientStub({
      replaceTools: vi.fn().mockResolvedValue([
        {
          name: "save_note",
          description: "保存一条本地笔记",
          parameters: {},
        },
        {
          name: "search_notes",
          description: "搜索本地笔记",
          parameters: {},
        },
      ]),
    });

    render(
      <HermesHubExampleApp
        dependencies={{
          client,
          noteStore: createNoteStoreStub(),
          authFlow: createAuthFlowStub(),
          initialConfig: TEST_CONFIG,
        }}
      />,
    );

    fireEvent.change(screen.getByLabelText("Hermes-Hub Base URL"), {
      target: { value: "https://hub.changed.example" },
    });
    fireEvent.click(screen.getByRole("button", { name: "保存配置" }));
    fireEvent.click(screen.getByRole("button", { name: "同步工具" }));

    await waitFor(() => {
      expect(client.replaceTools).toHaveBeenCalledWith(
        expect.objectContaining({
          baseUrl: "https://hub.changed.example",
        }),
        expect.arrayContaining([
          expect.objectContaining({ name: "save_note" }),
          expect.objectContaining({ name: "search_notes" }),
        ]),
      );
    });

    expect(localStorage.getItem(EXAMPLE_STORAGE_KEYS.config)).toContain(
      "https://hub.changed.example",
    );
    expect(screen.getByText("工具已同步")).toBeInTheDocument();
  });

  it("登录后能恢复用户态、创建会话并发送消息", async () => {
    const client = createClientStub({
      listSessions: vi.fn().mockResolvedValue([]),
      createSession: vi.fn().mockResolvedValue(TEST_SESSION),
      appendMessage: vi.fn().mockResolvedValue({
        id: "message-1",
        sessionId: TEST_SESSION.id,
        role: "user",
        messageKind: "text",
        content: "请把这条消息记下来",
        attachments: [],
        createdAt: 1,
        updatedAt: 1,
      }),
    });
    const authFlow = createAuthFlowStub({
      completeAuthorizationFromLocation: vi.fn().mockResolvedValue({
        accessToken: {
          accessToken: "oauth-token",
          tokenType: "Bearer",
          expiresIn: 604800,
          scope: "openid profile email",
        },
        userInfo: TEST_USER_INFO,
      }),
    });

    render(
      <HermesHubExampleApp
        dependencies={{
          client,
          noteStore: createNoteStoreStub(),
          authFlow,
          initialConfig: TEST_CONFIG,
        }}
      />,
    );

    await waitFor(() => {
      expect(authFlow.completeAuthorizationFromLocation).toHaveBeenCalled();
    });
    await waitFor(() => {
      expect(client.listSessions).toHaveBeenCalledWith(TEST_CONFIG, "oauth-token");
    });

    fireEvent.click(screen.getByRole("button", { name: "新建会话" }));
    await waitFor(() => {
      expect(client.createSession).toHaveBeenCalledWith(TEST_CONFIG, "oauth-token", {
        kind: "agent",
        title: "IndexedDB 笔记演示",
      });
    });

    fireEvent.change(screen.getByLabelText("输入消息"), {
      target: { value: "请把这条消息记下来" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => {
      expect(client.appendMessage).toHaveBeenCalledWith(
        TEST_CONFIG,
        "oauth-token",
        TEST_SESSION.id,
        expect.objectContaining({
          role: "user",
          content: "请把这条消息记下来",
        }),
      );
    });

    expect(screen.getAllByText("demo@example.com")).toHaveLength(2);
  });

  it("本地 token 过期时不会继续冒充已登录状态", async () => {
    localStorage.setItem(
      EXAMPLE_STORAGE_KEYS.accessToken,
      JSON.stringify({
        accessToken: "expired-token",
        tokenType: "Bearer",
        expiresIn: 1,
        scope: "openid profile email",
        createdAt: 1,
      }),
    );
    localStorage.setItem(
      EXAMPLE_STORAGE_KEYS.userInfo,
      JSON.stringify(TEST_USER_INFO),
    );

    render(
      <HermesHubExampleApp
        dependencies={{
          client: createClientStub(),
          noteStore: createNoteStoreStub(),
          authFlow: createAuthFlowStub(),
          initialConfig: TEST_CONFIG,
        }}
      />,
    );

    await waitFor(() => {
      expect(screen.getByText("本地 token 已过期，请重新登录")).toBeInTheDocument();
    });
    expect(localStorage.getItem(EXAMPLE_STORAGE_KEYS.accessToken)).toBeNull();
    expect(localStorage.getItem(EXAMPLE_STORAGE_KEYS.userInfo)).toBeNull();
  });

  it("切换会话时会立即清空上一个会话的消息", async () => {
    const anotherSession: ExampleSessionSummary = {
      ...TEST_SESSION,
      id: "session-2",
      title: "第二个会话",
      updatedAt: 2,
    };
    const client = createClientStub({
      listSessions: vi.fn().mockResolvedValue([TEST_SESSION, anotherSession]),
      appendMessage: vi.fn().mockResolvedValue({
        id: "message-1",
        sessionId: TEST_SESSION.id,
        role: "user",
        messageKind: "text",
        content: "这是 session-1 的消息",
        attachments: [],
        createdAt: 1,
        updatedAt: 1,
      }),
    });
    const authFlow = createAuthFlowStub({
      completeAuthorizationFromLocation: vi.fn().mockResolvedValue({
        accessToken: {
          accessToken: "oauth-token",
          tokenType: "Bearer",
          expiresIn: 604800,
          scope: "openid profile email",
        },
        userInfo: TEST_USER_INFO,
      }),
    });

    render(
      <HermesHubExampleApp
        dependencies={{
          client,
          noteStore: createNoteStoreStub(),
          authFlow,
          initialConfig: TEST_CONFIG,
        }}
      />,
    );

    await waitFor(() => {
      expect(client.listSessions).toHaveBeenCalled();
    });

    fireEvent.change(screen.getByLabelText("输入消息"), {
      target: { value: "这是 session-1 的消息" },
    });
    fireEvent.click(screen.getByRole("button", { name: "发送" }));

    await waitFor(() => {
      expect(screen.getByText("这是 session-1 的消息")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: /第二个会话/ }));

    await waitFor(() => {
      expect(screen.queryByText("这是 session-1 的消息")).not.toBeInTheDocument();
    });
  });

  it("OAuth 回调恢复完成前不会先用本地旧 token 启动会话事件订阅", async () => {
    const restoreDeferred = createDeferred<{
      accessToken: {
        accessToken: string;
        tokenType: string;
        expiresIn: number;
        scope: string;
      };
      userInfo: ExampleUserInfo;
    }>();
    localStorage.setItem(
      EXAMPLE_STORAGE_KEYS.accessToken,
      JSON.stringify({
        accessToken: "stale-token",
        tokenType: "Bearer",
        expiresIn: 604800,
        scope: "openid profile email",
        createdAt: Date.now(),
      }),
    );
    localStorage.setItem(
      EXAMPLE_STORAGE_KEYS.userInfo,
      JSON.stringify(TEST_USER_INFO),
    );
    localStorage.setItem(EXAMPLE_STORAGE_KEYS.selectedSessionId, TEST_SESSION.id);

    const subscribeSessionEvents = vi.fn().mockReturnValue(() => undefined);
    const client = createClientStub({
      listSessions: vi.fn().mockResolvedValue([TEST_SESSION]),
      subscribeSessionEvents,
    });
    const authFlow = createAuthFlowStub({
      completeAuthorizationFromLocation: vi.fn().mockImplementation(
        () => restoreDeferred.promise,
      ),
    });

    render(
      <HermesHubExampleApp
        dependencies={{
          client,
          noteStore: createNoteStoreStub(),
          authFlow,
          initialConfig: TEST_CONFIG,
        }}
      />,
    );

    await waitFor(() => {
      expect(authFlow.completeAuthorizationFromLocation).toHaveBeenCalledTimes(1);
    });
    expect(client.subscribeSessionEvents).not.toHaveBeenCalled();

    restoreDeferred.resolve({
      accessToken: {
        accessToken: "fresh-token",
        tokenType: "Bearer",
        expiresIn: 604800,
        scope: "openid profile email",
      },
      userInfo: TEST_USER_INFO,
    });

    await waitFor(() => {
      expect(subscribeSessionEvents).toHaveBeenCalledTimes(1);
    });
    expect(subscribeSessionEvents.mock.calls[0]?.[0]).toEqual(TEST_CONFIG);
    expect(subscribeSessionEvents.mock.calls[0]?.[1]).toBe("fresh-token");
    expect(subscribeSessionEvents.mock.calls[0]?.[2]).toBe(TEST_SESSION.id);
  });

  it("StrictMode 下 OAuth 回调并发恢复时不会回退到本地旧 token", async () => {
    const tokenDeferred = createDeferred<{
      access_token: string;
      token_type: string;
      expires_in: number;
      scope: string;
    }>();
    localStorage.setItem(
      EXAMPLE_STORAGE_KEYS.accessToken,
      JSON.stringify({
        accessToken: "stale-token",
        tokenType: "Bearer",
        expiresIn: 604800,
        scope: "openid profile email",
        createdAt: Date.now(),
      }),
    );
    localStorage.setItem(
      EXAMPLE_STORAGE_KEYS.userInfo,
      JSON.stringify(TEST_USER_INFO),
    );
    localStorage.setItem(EXAMPLE_STORAGE_KEYS.selectedSessionId, TEST_SESSION.id);
    localStorage.setItem(EXAMPLE_STORAGE_KEYS.oauthState, "state-1");
    window.history.pushState(
      {},
      "",
      "/examples/hermes-hub/?code=one-time-code&state=state-1",
    );

    const subscribeSessionEvents = vi.fn().mockReturnValue(() => undefined);
    const client = createClientStub({
      exchangeAuthorizationCode: vi.fn().mockImplementation(() => tokenDeferred.promise),
      getUserInfo: vi.fn().mockResolvedValue(TEST_USER_INFO),
      listSessions: vi.fn().mockResolvedValue([TEST_SESSION]),
      subscribeSessionEvents,
    });
    const authFlow = createExampleAuthFlow(client);

    render(
      <StrictMode>
        <HermesHubExampleApp
          dependencies={{
            client,
            noteStore: createNoteStoreStub(),
            authFlow,
            initialConfig: TEST_CONFIG,
          }}
        />
      </StrictMode>,
    );

    await waitFor(() => {
      expect(client.exchangeAuthorizationCode).toHaveBeenCalledTimes(1);
    });
    expect(subscribeSessionEvents).not.toHaveBeenCalled();

    tokenDeferred.resolve({
      access_token: "fresh-token",
      token_type: "Bearer",
      expires_in: 604800,
      scope: "openid profile email",
    });

    await waitFor(() => {
      expect(subscribeSessionEvents).toHaveBeenCalledTimes(1);
    });
    expect(subscribeSessionEvents.mock.calls[0]?.[1]).toBe("fresh-token");
    expect(subscribeSessionEvents.mock.calls[0]?.[2]).toBe(TEST_SESSION.id);
  });

  it("切换会话后会重置上一个会话残留的工具状态", async () => {
    const anotherSession: ExampleSessionSummary = {
      ...TEST_SESSION,
      id: "session-2",
      title: "第二个会话",
      updatedAt: 2,
    };
    let emitEvent: ((event: ExampleSessionEvent) => void) | null = null;
    const client = createClientStub({
      listSessions: vi.fn().mockResolvedValue([TEST_SESSION, anotherSession]),
      submitBusinessToolResult: vi.fn().mockResolvedValue({
        id: "message-2",
        sessionId: TEST_SESSION.id,
        role: "assistant",
        messageKind: "text",
        content: "done",
        attachments: [],
        createdAt: 2,
        updatedAt: 2,
      }),
      subscribeSessionEvents: vi.fn((_config, _token, _sessionId, onEvent) => {
        emitEvent = onEvent;
        return () => undefined;
      }),
    });
    const authFlow = createAuthFlowStub({
      completeAuthorizationFromLocation: vi.fn().mockResolvedValue({
        accessToken: {
          accessToken: "oauth-token",
          tokenType: "Bearer",
          expiresIn: 604800,
          scope: "openid profile email",
        },
        userInfo: TEST_USER_INFO,
      }),
    });

    render(
      <HermesHubExampleApp
        dependencies={{
          client,
          noteStore: createNoteStoreStub({
            saveNote: vi.fn().mockResolvedValue({
              id: "note-1",
              title: "未命名笔记",
              content: "记住这条信息",
              tags: [],
              createdAt: 1,
              updatedAt: 1,
            }),
          }),
          authFlow,
          initialConfig: TEST_CONFIG,
        }}
      />,
    );

    await waitFor(() => {
      expect(client.subscribeSessionEvents).toHaveBeenCalled();
    });

    if (!emitEvent) {
      throw new Error("expected session event subscriber");
    }
    const emitSessionEvent = emitEvent as (event: ExampleSessionEvent) => void;

    emitSessionEvent({
      type: "business_tool_request",
      request: {
        requestId: "req-1",
        sessionId: TEST_SESSION.id,
        integrationId: "crm",
        toolName: "save_note",
        arguments: {
          content: "记住这条信息",
        },
        timeoutSeconds: 60,
        expiresAt: 9_999_999_999,
        status: "pending",
        createdAt: 1,
        updatedAt: 1,
      },
    });

    await waitFor(() => {
      expect(screen.getByText("save_note: 已写入本地笔记库")).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: /第二个会话/ }));

    await waitFor(() => {
      expect(screen.queryByText("save_note: 已写入本地笔记库")).not.toBeInTheDocument();
    });
  });
});

function createClientStub(overrides: Partial<ExampleHubClient> = {}): ExampleHubClient {
  return {
    replaceTools: vi.fn().mockResolvedValue([]),
    exchangeAuthorizationCode: vi.fn(),
    getUserInfo: vi.fn(),
    listSessions: vi.fn().mockResolvedValue([]),
    createSession: vi.fn().mockResolvedValue(TEST_SESSION),
    deleteSession: vi.fn().mockResolvedValue(undefined),
    appendMessage: vi.fn().mockResolvedValue({
      id: "message-1",
      sessionId: TEST_SESSION.id,
      role: "user",
      messageKind: "text",
      content: "hello",
      attachments: [],
      createdAt: 1,
      updatedAt: 1,
    }),
    submitBusinessToolResult: vi.fn().mockResolvedValue({
      id: "message-2",
      sessionId: TEST_SESSION.id,
      role: "assistant",
      messageKind: "text",
      content: "done",
      attachments: [],
      createdAt: 2,
      updatedAt: 2,
    }),
    subscribeSessionEvents: vi.fn().mockReturnValue(() => undefined),
    ...overrides,
  };
}

function createNoteStoreStub(
  overrides: {
    saveNote?: ReturnType<typeof vi.fn>;
    searchNotes?: ReturnType<typeof vi.fn>;
    listNotes?: ReturnType<typeof vi.fn>;
  } = {},
) : ExampleNoteStore {
  return {
    saveNote: vi.fn(),
    searchNotes: vi.fn().mockResolvedValue([]),
    listNotes: vi.fn().mockResolvedValue([]),
    ...overrides,
  } as ExampleNoteStore;
}

function createAuthFlowStub(overrides: Record<string, unknown> = {}): ExampleAuthFlow {
  return {
    beginAuthorization: vi.fn(),
    completeAuthorizationFromLocation: vi.fn().mockResolvedValue(null),
    ...overrides,
  } as ExampleAuthFlow;
}

function createDeferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}
