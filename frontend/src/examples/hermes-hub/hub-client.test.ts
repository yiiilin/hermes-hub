import { waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  createHubClient,
  type ExampleHubConfig,
  type ExampleSessionEvent,
} from "./hub-client";

const TEST_CONFIG: ExampleHubConfig = {
  baseUrl: "https://hub.example/",
  clientId: "client-demo",
  clientSecret: "secret-demo",
  redirectUri: "https://app.example/examples/hermes-hub/",
  scopes: "openid profile email",
};

describe("hub client", () => {
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("使用 Basic 认证全量同步本地工具定义", async () => {
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(JSON.stringify({ tools: [] }), {
        status: 200,
        headers: {
          "Content-Type": "application/json",
        },
      }),
    );

    const client = createHubClient();

    await client.replaceTools(TEST_CONFIG, [
      {
        name: "save_note",
        description: "保存一条笔记",
        parameters: {
          type: "object",
          properties: {
            content: { type: "string" },
          },
          required: ["content"],
        },
      },
    ]);

    expect(fetchMock).toHaveBeenCalledTimes(1);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("https://hub.example/api/integrations/apps/self/tools");
    expect(init).toMatchObject({
      method: "PUT",
      headers: expect.objectContaining({
        Authorization: `Basic ${btoa("client-demo:secret-demo")}`,
        "Content-Type": "application/json",
      }),
    });
    expect(JSON.parse(String(init?.body))).toEqual({
      tools: [
        {
          name: "save_note",
          description: "保存一条笔记",
          parameters: {
            type: "object",
            properties: {
              content: { type: "string" },
            },
            required: ["content"],
          },
        },
      ],
    });
  });

  it("OAuth 与会话接口使用预期的 bearer 请求格式", async () => {
    const fetchMock = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            access_token: "oauth-token",
            token_type: "Bearer",
            expires_in: 604800,
            scope: "openid profile email",
          }),
          {
            status: 200,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            id: "user-1",
            sub: "user-1",
            email: "demo@example.com",
            integration_id: "crm",
            toolset_names: ["save_note", "search_notes"],
          }),
          {
            status: 200,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            session: {
              id: "session-1",
              title: "CRM case",
              is_home: false,
              deletable: true,
              hidden_from_web: true,
              created_at: 1,
              updated_at: 1,
            },
          }),
          {
            status: 201,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            message: {
              id: "message-1",
              session_id: "session-1",
              role: "user",
              message_kind: "text",
              content: "hello",
              attachments: [],
              created_at: 1,
              updated_at: 1,
            },
          }),
          {
            status: 201,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      )
      .mockResolvedValueOnce(
        new Response(
          JSON.stringify({
            message: {
              id: "message-2",
              session_id: "session-1",
              role: "assistant",
              message_kind: "text",
              content: "已保存笔记",
              attachments: [],
              created_at: 2,
              updated_at: 2,
            },
          }),
          {
            status: 201,
            headers: {
              "Content-Type": "application/json",
            },
          },
        ),
      );

    const client = createHubClient();

    await client.exchangeAuthorizationCode(TEST_CONFIG, "auth-code-1");
    await client.getUserInfo(TEST_CONFIG, "oauth-token");
    await client.createSession(TEST_CONFIG, "oauth-token", {
      kind: "agent",
      title: "CRM case",
    });
    await client.appendMessage(TEST_CONFIG, "oauth-token", "session-1", {
      role: "user",
      content: "hello",
      clientMessageKey: "client-1",
    });
    await client.submitBusinessToolResult(
      TEST_CONFIG,
      "oauth-token",
      "session-1",
      "req-1",
      "已保存笔记",
    );

    const tokenCall = fetchMock.mock.calls[0]!;
    expect(tokenCall[0]).toBe("https://hub.example/api/oauth/token");
    expect(tokenCall[1]).toMatchObject({
      method: "POST",
      headers: expect.objectContaining({
        "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
      }),
    });
    expect(String(tokenCall[1]?.body)).toContain("grant_type=authorization_code");
    expect(String(tokenCall[1]?.body)).toContain("client_id=client-demo");
    expect(String(tokenCall[1]?.body)).toContain("client_secret=secret-demo");
    expect(String(tokenCall[1]?.body)).toContain(
      "redirect_uri=https%3A%2F%2Fapp.example%2Fexamples%2Fhermes-hub%2F",
    );
    expect(String(tokenCall[1]?.body)).toContain("code=auth-code-1");

    const userInfoCall = fetchMock.mock.calls[1]!;
    expect(userInfoCall[0]).toBe("https://hub.example/api/oauth/userinfo");
    expect(userInfoCall[1]).toMatchObject({
      method: "GET",
      headers: expect.objectContaining({
        Authorization: "Bearer oauth-token",
      }),
    });

    const createSessionCall = fetchMock.mock.calls[2]!;
    expect(createSessionCall[0]).toBe("https://hub.example/api/integrations/sessions");
    expect(JSON.parse(String(createSessionCall[1]?.body))).toEqual({
      kind: "agent",
      title: "CRM case",
    });

    const appendMessageCall = fetchMock.mock.calls[3]!;
    expect(appendMessageCall[0]).toBe(
      "https://hub.example/api/integrations/sessions/session-1/messages",
    );
    expect(JSON.parse(String(appendMessageCall[1]?.body))).toEqual({
      role: "user",
      content: "hello",
      attachments: [],
      client_message_key: "client-1",
    });

    const toolResultCall = fetchMock.mock.calls[4]!;
    expect(toolResultCall[0]).toBe(
      "https://hub.example/api/integrations/sessions/session-1/business-tool-requests/req-1/result",
    );
    expect(JSON.parse(String(toolResultCall[1]?.body))).toEqual({
      result: "已保存笔记",
    });
  });

  it("能解析最小必要的 SSE 事件流", async () => {
    const encoder = new TextEncoder();
    const stream = new ReadableStream({
      start(controller) {
        controller.enqueue(
          encoder.encode(
            [
              "event: messages_snapshot",
              'data: {"type":"messages_snapshot","messages":[],"active_run":null,"session":{"id":"session-1","title":"演示","is_home":false,"deletable":true,"hidden_from_web":true,"created_at":1,"updated_at":1},"business_tool_requests":[]}',
              "",
              "event: business_tool_request",
              'data: {"type":"business_tool_request","request":{"request_id":"req-1","session_id":"session-1","integration_id":"crm","tool_name":"save_note","arguments":{"content":"hello"},"timeout_seconds":60,"expires_at":9999999999,"status":"pending","created_at":1,"updated_at":1}}',
              "",
            ].join("\n"),
          ),
        );
        controller.close();
      },
    });
    const fetchMock = vi.spyOn(globalThis, "fetch").mockResolvedValue(
      new Response(stream, {
        status: 200,
        headers: {
          "Content-Type": "text/event-stream",
        },
      }),
    );

    const client = createHubClient();
    const received: ExampleSessionEvent[] = [];
    const errors: Error[] = [];

    const stop = client.subscribeSessionEvents(
      TEST_CONFIG,
      "oauth-token",
      "session-1",
      (event) => {
        received.push(event);
      },
      (error) => {
        errors.push(error);
      },
      {
        reconnectDelayMs: 60_000,
      },
    );

    await waitFor(() => {
      expect(received).toHaveLength(2);
    });
    stop();

    expect(fetchMock).toHaveBeenCalledWith(
      "https://hub.example/api/integrations/sessions/session-1/events",
      expect.objectContaining({
        method: "GET",
        headers: expect.objectContaining({
          Accept: "text/event-stream",
          Authorization: "Bearer oauth-token",
        }),
      }),
    );
    expect(received[0]?.type).toBe("messages_snapshot");
    expect(received[1]).toMatchObject({
      type: "business_tool_request",
      request: {
        requestId: "req-1",
        toolName: "save_note",
      },
    });
    expect(errors).toHaveLength(1);
    expect(errors[0]?.message).toBe("session event stream disconnected");
  });
});
