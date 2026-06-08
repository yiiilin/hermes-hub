export type ExampleHubConfig = {
  baseUrl: string;
  clientId: string;
  clientSecret: string;
  redirectUri: string;
  scopes: string;
};

export type ExampleToolDefinition = {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
};

export type ExampleOAuthToken = {
  access_token: string;
  token_type: string;
  expires_in: number;
  scope: string;
};

export type ExampleUserInfo = {
  id: string;
  sub: string;
  email: string;
  integration_id: string;
  toolset_names: string[];
};

export type ExampleAttachment = {
  id?: string;
  name: string;
  contentType: string;
  kind: "file" | "image";
  size?: number;
  downloadUrl?: string;
  dataUrl?: string;
};

export type ExampleMessage = {
  id: string;
  sessionId: string;
  role: "user" | "assistant";
  messageKind: "text" | "execution";
  clientMessageKey?: string | null;
  content: string;
  attachments: ExampleAttachment[];
  createdAt: number;
  updatedAt: number;
};

export type ExampleSessionSummary = {
  id: string;
  title?: string | null;
  isHome: boolean;
  deletable: boolean;
  hiddenFromWeb: boolean;
  createdAt: number;
  updatedAt: number;
};

export type ExampleActiveRun = {
  runId: string;
  status: string;
  output?: string | null;
  error?: string | null;
  outputMessageId?: string | null;
  createdAt?: number;
  updatedAt?: number;
};

export type ExampleChannelRun = {
  id: string;
  runId: string;
  sessionId: string;
  userMessageId?: string | null;
  status: string;
  input: string;
  inputAttachments: ExampleAttachment[];
  outputMessageId?: string | null;
  error?: string | null;
  createdAt?: number;
  updatedAt?: number;
  completedAt?: number | null;
};

export type ExampleBusinessToolRequest = {
  requestId: string;
  sessionId: string;
  integrationId: string;
  toolName: string;
  arguments: Record<string, unknown>;
  timeoutSeconds: number;
  expiresAt: number;
  status: "pending" | "completed" | "failed" | "expired";
  createdAt: number;
  updatedAt: number;
  resultMessageId?: string | null;
};

export type ExampleBusinessToolRequestEvent = {
  type: "business_tool_request";
  request: ExampleBusinessToolRequest;
};

export type ExampleSessionEvent =
  | {
      type: "messages_snapshot";
      messages: ExampleMessage[];
      activeRun: ExampleActiveRun | null;
      session?: ExampleSessionSummary;
      businessToolRequests: ExampleBusinessToolRequestEvent[];
    }
  | { type: "message_created"; message: ExampleMessage }
  | { type: "message_updated"; message: ExampleMessage }
  | { type: "session_updated"; session: ExampleSessionSummary }
  | { type: "run_updated"; run: ExampleChannelRun }
  | { type: "run_cleared"; sessionId: string }
  | { type: "session_deleted"; sessionId: string }
  | ExampleBusinessToolRequestEvent;

export type ExampleSessionStreamOptions = {
  reconnectDelayMs?: number;
};

export type ExampleHubClient = {
  replaceTools: (
    config: ExampleHubConfig,
    tools: ExampleToolDefinition[],
  ) => Promise<ExampleToolDefinition[]>;
  exchangeAuthorizationCode: (
    config: ExampleHubConfig,
    code: string,
  ) => Promise<ExampleOAuthToken>;
  getUserInfo: (config: ExampleHubConfig, accessToken: string) => Promise<ExampleUserInfo>;
  listSessions: (
    config: ExampleHubConfig,
    accessToken: string,
  ) => Promise<ExampleSessionSummary[]>;
  createSession: (
    config: ExampleHubConfig,
    accessToken: string,
    input: { kind: "agent" | "chat"; title?: string },
  ) => Promise<ExampleSessionSummary>;
  deleteSession: (
    config: ExampleHubConfig,
    accessToken: string,
    sessionId: string,
  ) => Promise<void>;
  appendMessage: (
    config: ExampleHubConfig,
    accessToken: string,
    sessionId: string,
    input: {
      role: "user" | "assistant";
      content: string;
      attachments?: ExampleAttachment[];
      clientMessageKey?: string;
    },
  ) => Promise<ExampleMessage>;
  submitBusinessToolResult: (
    config: ExampleHubConfig,
    accessToken: string,
    sessionId: string,
    requestId: string,
    result: string,
  ) => Promise<ExampleMessage>;
  subscribeSessionEvents: (
    config: ExampleHubConfig,
    accessToken: string,
    sessionId: string,
    onEvent: (event: ExampleSessionEvent) => void,
    onError?: (error: Error) => void,
    options?: ExampleSessionStreamOptions,
  ) => () => void;
};

type JsonRequestOptions = {
  method?: string;
  headers?: HeadersInit;
  body?: BodyInit;
};

function encodeBasicAuth(clientId: string, clientSecret: string): string {
  return `Basic ${btoa(`${clientId}:${clientSecret}`)}`;
}

function buildUrl(baseUrl: string, path: string): string {
  return new URL(path, baseUrl.endsWith("/") ? baseUrl : `${baseUrl}/`).toString();
}

async function requestJson<T>(
  config: ExampleHubConfig,
  path: string,
  options: JsonRequestOptions,
): Promise<T> {
  const response = await fetch(buildUrl(config.baseUrl, path), options);
  if (!response.ok) {
    throw await createRequestError(response);
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

async function createRequestError(response: Response): Promise<Error> {
  const contentType = response.headers.get("Content-Type") ?? "";
  if (contentType.includes("application/json")) {
    const payload = (await response.json().catch(() => null)) as
      | { message?: string; error?: string }
      | null;
    return new Error(payload?.message ?? payload?.error ?? response.statusText);
  }
  const message = await response.text().catch(() => response.statusText);
  return new Error(message || response.statusText);
}

function mapAttachment(raw: Record<string, unknown>): ExampleAttachment {
  return {
    id: typeof raw.id === "string" ? raw.id : undefined,
    name: typeof raw.name === "string" ? raw.name : "",
    contentType: typeof raw.content_type === "string" ? raw.content_type : "",
    kind: raw.kind === "image" ? "image" : "file",
    size: typeof raw.size === "number" ? raw.size : undefined,
    downloadUrl: typeof raw.download_url === "string" ? raw.download_url : undefined,
    dataUrl: typeof raw.data_url === "string" ? raw.data_url : undefined,
  };
}

function mapMessage(raw: Record<string, unknown>): ExampleMessage {
  return {
    id: String(raw.id ?? ""),
    sessionId: String(raw.session_id ?? ""),
    role: raw.role === "assistant" ? "assistant" : "user",
    messageKind: raw.message_kind === "execution" ? "execution" : "text",
    clientMessageKey:
      typeof raw.client_message_key === "string" ? raw.client_message_key : undefined,
    content: String(raw.content ?? ""),
    attachments: Array.isArray(raw.attachments)
      ? raw.attachments
          .filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object")
          .map(mapAttachment)
      : [],
    createdAt: typeof raw.created_at === "number" ? raw.created_at : 0,
    updatedAt: typeof raw.updated_at === "number" ? raw.updated_at : 0,
  };
}

function mapSession(raw: Record<string, unknown>): ExampleSessionSummary {
  return {
    id: String(raw.id ?? ""),
    title: typeof raw.title === "string" ? raw.title : null,
    isHome: raw.is_home === true,
    deletable: raw.deletable !== false,
    hiddenFromWeb: raw.hidden_from_web === true,
    createdAt: typeof raw.created_at === "number" ? raw.created_at : 0,
    updatedAt: typeof raw.updated_at === "number" ? raw.updated_at : 0,
  };
}

function mapActiveRun(raw: Record<string, unknown>): ExampleActiveRun {
  return {
    runId: String(raw.run_id ?? ""),
    status: String(raw.status ?? ""),
    output: typeof raw.output === "string" ? raw.output : null,
    error: typeof raw.error === "string" ? raw.error : null,
    outputMessageId:
      typeof raw.output_message_id === "string" ? raw.output_message_id : null,
    createdAt: typeof raw.created_at === "number" ? raw.created_at : undefined,
    updatedAt: typeof raw.updated_at === "number" ? raw.updated_at : undefined,
  };
}

function mapRun(raw: Record<string, unknown>): ExampleChannelRun {
  return {
    id: String(raw.id ?? ""),
    runId: String(raw.run_id ?? ""),
    sessionId: String(raw.session_id ?? ""),
    userMessageId: typeof raw.user_message_id === "string" ? raw.user_message_id : null,
    status: String(raw.status ?? ""),
    input: String(raw.input ?? ""),
    inputAttachments: Array.isArray(raw.input_attachments)
      ? raw.input_attachments
          .filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object")
          .map(mapAttachment)
      : [],
    outputMessageId:
      typeof raw.output_message_id === "string" ? raw.output_message_id : null,
    error: typeof raw.error === "string" ? raw.error : null,
    createdAt: typeof raw.created_at === "number" ? raw.created_at : undefined,
    updatedAt: typeof raw.updated_at === "number" ? raw.updated_at : undefined,
    completedAt: typeof raw.completed_at === "number" ? raw.completed_at : null,
  };
}

function mapBusinessToolRequest(
  raw: Record<string, unknown>,
): ExampleBusinessToolRequestEvent {
  return {
    type: "business_tool_request",
    request: {
      requestId: String(raw.request_id ?? raw.requestId ?? ""),
      sessionId: String(raw.session_id ?? raw.sessionId ?? ""),
      integrationId: String(raw.integration_id ?? raw.integrationId ?? ""),
      toolName: String(raw.tool_name ?? raw.toolName ?? ""),
      arguments:
        raw.arguments && typeof raw.arguments === "object" && !Array.isArray(raw.arguments)
          ? (raw.arguments as Record<string, unknown>)
          : {},
      timeoutSeconds:
        typeof raw.timeout_seconds === "number"
          ? raw.timeout_seconds
          : typeof raw.timeoutSeconds === "number"
            ? raw.timeoutSeconds
            : 0,
      expiresAt:
        typeof raw.expires_at === "number"
          ? raw.expires_at
          : typeof raw.expiresAt === "number"
            ? raw.expiresAt
            : 0,
      status:
        raw.status === "completed" ||
        raw.status === "failed" ||
        raw.status === "expired"
          ? raw.status
          : "pending",
      createdAt:
        typeof raw.created_at === "number"
          ? raw.created_at
          : typeof raw.createdAt === "number"
            ? raw.createdAt
            : 0,
      updatedAt:
        typeof raw.updated_at === "number"
          ? raw.updated_at
          : typeof raw.updatedAt === "number"
            ? raw.updatedAt
            : 0,
      resultMessageId:
        typeof raw.result_message_id === "string"
          ? raw.result_message_id
          : typeof raw.resultMessageId === "string"
            ? raw.resultMessageId
            : null,
    },
  };
}

function mapSessionEvent(name: string, payload: Record<string, unknown>): ExampleSessionEvent {
  if (name === "messages_snapshot") {
    const businessToolRequests = Array.isArray(payload.business_tool_requests)
      ? payload.business_tool_requests
          .filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object")
          .map((item) => {
            const request =
              item.request && typeof item.request === "object" ? (item.request as Record<string, unknown>) : item;
            return mapBusinessToolRequest(request);
          })
      : [];
    return {
      type: "messages_snapshot",
      messages: Array.isArray(payload.messages)
        ? payload.messages
            .filter((item): item is Record<string, unknown> => Boolean(item) && typeof item === "object")
            .map(mapMessage)
        : [],
      activeRun:
        payload.active_run && typeof payload.active_run === "object"
          ? mapActiveRun(payload.active_run as Record<string, unknown>)
          : null,
      session:
        payload.session && typeof payload.session === "object"
          ? mapSession(payload.session as Record<string, unknown>)
          : undefined,
      businessToolRequests,
    };
  }
  if (name === "message_created" || name === "message_updated") {
    return {
      type: name,
      message: mapMessage(payload.message as Record<string, unknown>),
    };
  }
  if (name === "session_updated") {
    return {
      type: "session_updated",
      session: mapSession(payload.session as Record<string, unknown>),
    };
  }
  if (name === "run_updated") {
    return {
      type: "run_updated",
      run: mapRun(payload.run as Record<string, unknown>),
    };
  }
  if (name === "run_cleared") {
    return {
      type: "run_cleared",
      sessionId: String(payload.session_id ?? ""),
    };
  }
  if (name === "session_deleted") {
    return {
      type: "session_deleted",
      sessionId: String(payload.session_id ?? ""),
    };
  }
  if (name === "business_tool_request") {
    const request =
      payload.request && typeof payload.request === "object"
        ? (payload.request as Record<string, unknown>)
        : payload;
    return mapBusinessToolRequest(request);
  }
  throw new Error(`unsupported session event: ${name}`);
}

async function readSseStream(
  response: Response,
  onEvent: (eventName: string, payload: Record<string, unknown>) => void,
) {
  const reader = response.body?.getReader();
  if (!reader) {
    throw new Error("SSE 响应没有可读流");
  }
  const decoder = new TextDecoder();
  let buffer = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }
    buffer += decoder.decode(value, { stream: true });
    const segments = buffer.split(/\r?\n\r?\n/);
    buffer = segments.pop() ?? "";
    for (const segment of segments) {
      emitParsedSseChunk(segment, onEvent);
    }
  }

  if (buffer.trim()) {
    emitParsedSseChunk(buffer, onEvent);
  }
}

function parseSseChunk(chunk: string): { eventName: string | null; data: string } | null {
  const lines = chunk.split(/\r?\n/);
  let eventName: string | null = null;
  const dataLines: string[] = [];

  for (const line of lines) {
    if (!line || line.startsWith(":")) {
      continue;
    }
    if (line.startsWith("event:")) {
      eventName = line.slice("event:".length).trim();
      continue;
    }
    if (line.startsWith("data:")) {
      dataLines.push(line.slice("data:".length).trim());
    }
  }

  if (!eventName && dataLines.length === 0) {
    return null;
  }
  return {
    eventName,
    data: dataLines.join("\n"),
  };
}

function emitParsedSseChunk(
  chunk: string,
  onEvent: (eventName: string, payload: Record<string, unknown>) => void,
) {
  const parsed = parseSseChunk(chunk);
  if (!parsed?.eventName || !parsed.data) {
    return;
  }
  onEvent(parsed.eventName, JSON.parse(parsed.data) as Record<string, unknown>);
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms);
  });
}

export function createHubClient(): ExampleHubClient {
  return {
    async replaceTools(config, tools) {
      const payload = await requestJson<{ tools: ExampleToolDefinition[] }>(
        config,
        "/api/integrations/apps/self/tools",
        {
          method: "PUT",
          headers: {
            Authorization: encodeBasicAuth(config.clientId, config.clientSecret),
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ tools }),
        },
      );
      return payload.tools;
    },
    async exchangeAuthorizationCode(config, code) {
      const body = new URLSearchParams({
        grant_type: "authorization_code",
        client_id: config.clientId,
        client_secret: config.clientSecret,
        redirect_uri: config.redirectUri,
        code,
      });
      return requestJson<ExampleOAuthToken>(config, "/api/oauth/token", {
        method: "POST",
        headers: {
          "Content-Type": "application/x-www-form-urlencoded;charset=UTF-8",
        },
        body,
      });
    },
    async getUserInfo(config, accessToken) {
      return requestJson<ExampleUserInfo>(config, "/api/oauth/userinfo", {
        method: "GET",
        headers: {
          Authorization: `Bearer ${accessToken}`,
        },
      });
    },
    async listSessions(config, accessToken) {
      const payload = await requestJson<{ sessions: Array<Record<string, unknown>> }>(
        config,
        "/api/integrations/sessions",
        {
          method: "GET",
          headers: {
            Authorization: `Bearer ${accessToken}`,
          },
        },
      );
      return payload.sessions.map(mapSession);
    },
    async createSession(config, accessToken, input) {
      const payload = await requestJson<{ session: Record<string, unknown> }>(
        config,
        "/api/integrations/sessions",
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify(input),
        },
      );
      return mapSession(payload.session);
    },
    async deleteSession(config, accessToken, sessionId) {
      await requestJson<void>(config, `/api/integrations/sessions/${sessionId}`, {
        method: "DELETE",
        headers: {
          Authorization: `Bearer ${accessToken}`,
        },
      });
    },
    async appendMessage(config, accessToken, sessionId, input) {
      const payload = await requestJson<{ message: Record<string, unknown> }>(
        config,
        `/api/integrations/sessions/${sessionId}/messages`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({
            role: input.role,
            content: input.content,
            attachments: input.attachments ?? [],
            client_message_key: input.clientMessageKey,
          }),
        },
      );
      return mapMessage(payload.message);
    },
    async submitBusinessToolResult(config, accessToken, sessionId, requestId, result) {
      const payload = await requestJson<{ message: Record<string, unknown> }>(
        config,
        `/api/integrations/sessions/${sessionId}/business-tool-requests/${requestId}/result`,
        {
          method: "POST",
          headers: {
            Authorization: `Bearer ${accessToken}`,
            "Content-Type": "application/json",
          },
          body: JSON.stringify({ result }),
        },
      );
      return mapMessage(payload.message);
    },
    subscribeSessionEvents(config, accessToken, sessionId, onEvent, onError, options) {
      const reconnectDelayMs = options?.reconnectDelayMs ?? 1_500;
      const controller = new AbortController();
      let stopped = false;

      void (async () => {
        while (!stopped) {
          try {
            const response = await fetch(
              buildUrl(config.baseUrl, `/api/integrations/sessions/${sessionId}/events`),
              {
                method: "GET",
                headers: {
                  Accept: "text/event-stream",
                  Authorization: `Bearer ${accessToken}`,
                },
                signal: controller.signal,
              },
            );
            if (!response.ok) {
              throw await createRequestError(response);
            }
            await readSseStream(response, (eventName, payload) => {
              onEvent(mapSessionEvent(eventName, payload));
            });
            if (!stopped) {
              onError?.(new Error("session event stream disconnected"));
            }
          } catch (error) {
            if (controller.signal.aborted || stopped) {
              break;
            }
            onError?.(
              error instanceof Error ? error : new Error("session event stream disconnected"),
            );
          }
          if (stopped) {
            break;
          }
          await delay(reconnectDelayMs);
        }
      })();

      return () => {
        stopped = true;
        controller.abort();
      };
    },
  };
}
