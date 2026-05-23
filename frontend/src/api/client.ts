export type User = {
  id: string;
  email: string;
  role: "admin" | "user";
  status: "active" | "disabled";
};

export type Invite = {
  id: string;
  status: "pending" | "used" | "revoked" | "expired" | "exhausted";
  expires_at: number;
  max_uses: number;
  used_count: number;
};

export type Channel = {
  id: string;
  name: string;
  description?: string | null;
};

export type ChannelSession = {
  id: string;
  channel_id: string;
  kind: "chat" | "agent";
  title?: string | null;
  created_at?: number;
  updated_at?: number;
};

export type ChannelMessage = {
  id: string;
  session_id: string;
  role: "user" | "assistant";
  client_message_key?: string | null;
  content: string;
  attachments: HermesAttachment[];
  created_at: number;
};

export type ChannelRun = {
  id: string;
  run_id: string;
  session_id: string;
  user_message_id?: string | null;
  status: string;
  input: string;
  input_attachments: HermesAttachment[];
  output_message_id?: string | null;
  error?: string | null;
  created_at?: number;
  updated_at?: number;
  completed_at?: number | null;
};

export type HermesInstance = {
  id: string;
  user_id: string;
  kind: "external" | "managed_docker";
  status: "provisioning" | "running" | "stopped" | "error";
  name?: string;
  base_url: string;
  container_id?: string | null;
  health_status?: string;
};

export type ModelConfigKind = "llm" | "image" | "title";
export type ModelApiType = "chat_completions" | "responses" | "images_generations";
export type ReasoningEffort = "minimal" | "low" | "medium" | "high";

export type ModelConfig = {
  config_kind: ModelConfigKind;
  provider_name: string;
  provider_base_url: string;
  provider_api_key?: string;
  default_model: string;
  allowed_models: string[];
  api_type: ModelApiType;
  reasoning_effort?: ReasoningEffort | null;
  allow_streaming: boolean;
  request_timeout_seconds: number;
};

export type ModelConfigStatus = {
  model_config: ModelConfig;
  model_configs: ModelConfig[];
  required_models_ready: boolean;
  missing_required_model_config_kinds: ModelConfigKind[];
};

export type ModelConfigTestResult = {
  ok: boolean;
  status_code: number;
  message: string;
  duration_ms: number;
};

export type SystemSettings = {
  max_sessions_per_user: number;
  oidc: OidcSettings;
};

export type OidcSettings = {
  enabled: boolean;
  display_name: string;
  client_id: string;
  client_secret: string;
  issuer_url: string;
  authorization_url: string;
  token_url: string;
  userinfo_url: string;
  logout_url: string;
  scopes: string;
  username_claim: string;
  email_claim: string;
  allow_password_login: boolean;
  auto_create_users: boolean;
};

export type OidcPublicConfig = {
  enabled: boolean;
  display_name: string;
};

export type HermesAttachment = {
  id?: string;
  name: string;
  content_type: string;
  kind: "file" | "image";
  size?: number;
  download_url?: string;
  data_url?: string;
};

export type HermesActiveRun = {
  run_id: string;
  status: string;
  output?: string | null;
  error?: string | null;
  output_message_id?: string | null;
  created_at?: number;
  updated_at?: number;
};

export type ChannelSessionEvent =
  | {
      type: "messages_snapshot";
      messages: ChannelMessage[];
      active_run: HermesActiveRun | null;
    }
  | { type: "message_created"; message: ChannelMessage }
  | { type: "message_updated"; message: ChannelMessage }
  | { type: "run_updated"; run: ChannelRun }
  | { type: "run_cleared"; session_id: string }
  | { type: "session_deleted"; session_id: string };

export type HermesVerboseEvent = {
  kind:
    | "text"
    | "approval.request"
    | "approval.responded"
    | "tool.started"
    | "tool.completed"
    | "tool.progress"
    | "tool.call";
  tool?: string;
  detail?: string;
  choice?: string;
  failed?: boolean;
};

type HermesRunStarted = {
  run_id?: string;
  status?: string;
};

type HermesRunEvent = {
  event?: string;
  type?: string;
  delta?: string;
  output?: string;
  error?: string | boolean | { message?: string };
  message?: string;
  status?: string;
  tool?: string;
  name?: string;
  choice?: string;
  resolved?: number;
  preview?: string;
  command?: string;
  description?: string;
  text?: string;
  duration?: number;
  item?: {
    type?: string;
    name?: string;
    status?: string;
    arguments?: string;
    input?: string;
    output?: string;
    content?: string;
  };
};

export type HermesStreamHandlers = {
  onRunStarted?: (runId: string) => void;
  onDelta?: (delta: string) => void;
  onOutput?: (output: string) => void;
  onVerbose?: (message: HermesVerboseEvent | string) => void;
};

type HermesStreamProgress = {
  receivedBytes: number;
  deltaText: string;
  completedOutput: string;
  pendingEventName: string;
  pendingDataLines: string[];
};

class HermesRunFailedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "HermesRunFailedError";
  }
}

export type CreateInviteInput = {
  expires_at: number;
  max_uses: number;
};

export type ApiClient = {
  me: () => Promise<User | null>;
  bootstrapStatus: () => Promise<{ bootstrap_open: boolean }>;
  oidcConfig: () => Promise<OidcPublicConfig>;
  login: (email: string, password: string) => Promise<User>;
  bootstrapRegister: (email: string, password: string) => Promise<User>;
  registerWithInvite: (
    inviteToken: string,
    email: string,
    password: string,
  ) => Promise<User>;
  logout: () => Promise<void>;
  listUsers: () => Promise<User[]>;
  disableUser: (userId: string) => Promise<User>;
  enableUser: (userId: string) => Promise<User>;
  listInvites: () => Promise<Invite[]>;
  createInvite: (input: CreateInviteInput) => Promise<{ token: string; invite: Invite }>;
  revokeInvite: (inviteId: string) => Promise<Invite>;
  listHermesInstances: () => Promise<HermesInstance[]>;
  createHermesInstance: (userId: string) => Promise<HermesInstance>;
  startHermesInstance: (userId: string) => Promise<HermesInstance>;
  stopHermesInstance: (userId: string) => Promise<HermesInstance>;
  rebuildHermesInstance: (userId: string) => Promise<HermesInstance>;
  updateExternalHermesInstanceConfig: (
    userId: string,
    input: { name: string; base_url: string; api_token?: string },
  ) => Promise<HermesInstance>;
  listChannels: () => Promise<Channel[]>;
  createChannel: (name: string, description?: string) => Promise<Channel>;
  listSessions: (channelId: string) => Promise<ChannelSession[]>;
  createSession: (
    channelId: string,
    kind: "chat" | "agent",
    title?: string,
  ) => Promise<ChannelSession>;
  deleteSession: (channelId: string, sessionId: string) => Promise<void>;
  listSessionMessages: (channelId: string, sessionId: string) => Promise<ChannelMessage[]>;
  appendSessionMessage: (
    channelId: string,
    sessionId: string,
    input: {
      role: "user" | "assistant";
      content: string;
      attachments?: HermesAttachment[];
      clientMessageKey?: string;
    },
  ) => Promise<ChannelMessage>;
  updateSessionMessage: (
    channelId: string,
    sessionId: string,
    messageId: string,
    input: {
      content: string;
      attachments?: HermesAttachment[];
    },
  ) => Promise<ChannelMessage>;
  uploadSessionAttachments: (
    channelId: string,
    sessionId: string,
    files: File[],
  ) => Promise<HermesAttachment[]>;
  createChannelRun: (
    channelId: string,
    sessionId: string,
    input: {
      content: string;
      attachments?: HermesAttachment[];
      clientMessageKey?: string;
    },
  ) => Promise<{ message: ChannelMessage; run: ChannelRun }>;
  generateSessionTitle: (
    channelId: string,
    sessionId: string,
    prompt: string,
  ) => Promise<ChannelSession>;
  workspaceStatus: () => Promise<HermesInstance | null>;
  ensureHermes: () => Promise<HermesInstance>;
  modelConfig: () => Promise<ModelConfig>;
  modelConfigStatus: () => Promise<ModelConfigStatus>;
  modelConfigs: () => Promise<ModelConfig[]>;
  updateModelConfig: (config: ModelConfig) => Promise<void>;
  updateModelConfigs: (configs: ModelConfig[]) => Promise<void>;
  testModelConfig: (config: ModelConfig) => Promise<ModelConfigTestResult>;
  systemSettings: () => Promise<SystemSettings>;
  updateSystemSettings: (settings: SystemSettings) => Promise<void>;
  sendHermesPrompt: (
    prompt: string,
    attachments?: HermesAttachment[],
    sessionId?: string,
    handlers?: HermesStreamHandlers,
  ) => Promise<string>;
  activeHermesRun: (channelId: string, sessionId: string) => Promise<HermesActiveRun | null>;
  subscribeSessionEvents: (
    channelId: string,
    sessionId: string,
    onEvent: (event: ChannelSessionEvent) => void,
    onError?: (error: Error) => void,
  ) => () => void;
  stopHermesRun: (channelId: string, sessionId: string) => Promise<HermesActiveRun | null>;
  clearHermesRun: (channelId: string, sessionId: string) => Promise<void>;
  resumeHermesRun: (runId: string, handlers?: HermesStreamHandlers) => Promise<string>;
};

type RequestOptions = {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  body?: unknown;
  allowUnauthorized?: boolean;
};

type ApiErrorPayload = {
  error?: string;
  message?: string;
  max_sessions_per_user?: number;
};

// 保留后端错误码和参数，页面才能按当前语言生成用户可读提示。
export class ApiRequestError extends Error {
  readonly code?: string;
  readonly maxSessionsPerUser?: number;

  constructor(message: string, payload: ApiErrorPayload = {}) {
    super(message);
    this.name = "ApiRequestError";
    this.code = payload.error;
    this.maxSessionsPerUser = payload.max_sessions_per_user;
  }
}

async function request<T>(path: string, options: RequestOptions = {}): Promise<T> {
  const response = await fetch(path, {
    method: options.method ?? "GET",
    credentials: "include",
    headers:
      options.body === undefined
        ? undefined
        : {
            "Content-Type": "application/json",
          },
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });

  if (response.status === 401 && options.allowUnauthorized) {
    return null as T;
  }

  if (!response.ok) {
    const payload: ApiErrorPayload = await response
      .json()
      .then((value): ApiErrorPayload =>
        value && typeof value === "object"
          ? (value as ApiErrorPayload)
          : { message: response.statusText },
      )
      .catch((): ApiErrorPayload => ({ message: response.statusText }));
    const message = payload.message ?? payload.error ?? response.statusText;
    throw new ApiRequestError(String(message), payload);
  }

  if (response.status === 204) {
    return undefined as T;
  }

  return response.json() as Promise<T>;
}

async function updateModelConfigRequest(config: ModelConfig): Promise<void> {
  await request<void>("/api/admin/model-config", {
    method: "PUT",
    body: normalizedModelConfig(config),
  });
}

function normalizedModelConfig(config: ModelConfig): ModelConfig {
  return {
    ...config,
    api_type: config.config_kind === "image" ? "images_generations" : config.api_type,
    reasoning_effort: config.config_kind === "image" ? null : config.reasoning_effort,
    allowed_models: [config.default_model],
  };
}

export function defaultOidcSettings(): OidcSettings {
  return {
    enabled: false,
    display_name: "OpenID Connect",
    client_id: "",
    client_secret: "",
    issuer_url: "",
    authorization_url: "",
    token_url: "",
    userinfo_url: "",
    logout_url: "",
    scopes: "openid profile email",
    username_claim: "preferred_username",
    email_claim: "email",
    allow_password_login: true,
    auto_create_users: true,
  };
}

export function createApiClient(): ApiClient {
  return {
    async me() {
      const payload = await request<{ user: User } | null>("/api/auth/me", {
        allowUnauthorized: true,
      });
      return payload?.user ?? null;
    },
    async bootstrapStatus() {
      return request<{ bootstrap_open: boolean }>("/api/auth/bootstrap-status");
    },
    async oidcConfig() {
      const payload = await request<{ oidc: OidcPublicConfig }>("/api/auth/oidc/config");
      return payload.oidc;
    },
    async login(email, password) {
      const payload = await request<{ user: User }>("/api/auth/login", {
        method: "POST",
        body: { email, password },
      });
      return payload.user;
    },
    async bootstrapRegister(email, password) {
      const payload = await request<{ user: User }>("/api/auth/bootstrap-register", {
        method: "POST",
        body: { email, password },
      });
      return payload.user;
    },
    async registerWithInvite(inviteToken, email, password) {
      const payload = await request<{ user: User }>("/api/auth/register", {
        method: "POST",
        body: { invite_token: inviteToken, email, password },
      });
      return payload.user;
    },
    async logout() {
      await request<void>("/api/auth/logout", { method: "POST" });
    },
    async listUsers() {
      const payload = await request<{ users: User[] }>("/api/admin/users");
      return payload.users;
    },
    async disableUser(userId) {
      const payload = await request<{ user: User }>(
        `/api/admin/users/${userId}/disable`,
        { method: "POST" },
      );
      return payload.user;
    },
    async enableUser(userId) {
      const payload = await request<{ user: User }>(
        `/api/admin/users/${userId}/enable`,
        { method: "POST" },
      );
      return payload.user;
    },
    async listInvites() {
      const payload = await request<{ invites: Invite[] }>("/api/invites");
      return payload.invites;
    },
    async createInvite(input) {
      return request<{ token: string; invite: Invite }>("/api/invites", {
        method: "POST",
        body: input,
      });
    },
    async revokeInvite(inviteId) {
      const payload = await request<{ invite: Invite }>(
        `/api/invites/${inviteId}/revoke`,
        { method: "POST" },
      );
      return payload.invite;
    },
    async listHermesInstances() {
      const payload = await request<{ hermes_instances: HermesInstance[] }>(
        "/api/admin/hermes-instances",
      );
      return payload.hermes_instances;
    },
    async createHermesInstance(userId) {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        `/api/admin/users/${userId}/hermes-instance/create-managed`,
        { method: "POST" },
      );
      return payload.hermes_instance;
    },
    async startHermesInstance(userId) {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        `/api/admin/users/${userId}/hermes-instance/start`,
        { method: "POST" },
      );
      return payload.hermes_instance;
    },
    async stopHermesInstance(userId) {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        `/api/admin/users/${userId}/hermes-instance/stop`,
        { method: "POST" },
      );
      return payload.hermes_instance;
    },
    async rebuildHermesInstance(userId) {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        `/api/admin/users/${userId}/hermes-instance/rebuild-managed`,
        { method: "POST" },
      );
      return payload.hermes_instance;
    },
    async updateExternalHermesInstanceConfig(userId, input) {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        `/api/admin/users/${userId}/hermes-instance/external-config`,
        { method: "PUT", body: input },
      );
      return payload.hermes_instance;
    },
    async listChannels() {
      const payload = await request<{ channels: Channel[] }>("/api/channels");
      return payload.channels;
    },
    async createChannel(name, description) {
      const payload = await request<{ channel: Channel }>("/api/channels", {
        method: "POST",
        body: { name, description },
      });
      return payload.channel;
    },
    async listSessions(channelId) {
      const payload = await request<{ sessions: ChannelSession[] }>(
        `/api/channels/${channelId}/sessions`,
      );
      return payload.sessions;
    },
    async createSession(channelId, kind, title) {
      const payload = await request<{ session: ChannelSession }>(
        `/api/channels/${channelId}/sessions`,
        { method: "POST", body: { kind, title } },
      );
      return payload.session;
    },
    async deleteSession(channelId, sessionId) {
      await request<void>(`/api/channels/${channelId}/sessions/${sessionId}`, {
        method: "DELETE",
      });
    },
    async listSessionMessages(channelId, sessionId) {
      const payload = await request<{ messages: ChannelMessage[] }>(
        `/api/channels/${channelId}/sessions/${sessionId}/messages`,
      );
      return payload.messages;
    },
    async appendSessionMessage(channelId, sessionId, input) {
      const payload = await request<{ message: ChannelMessage }>(
        `/api/channels/${channelId}/sessions/${sessionId}/messages`,
        {
          method: "POST",
          body: {
            role: input.role,
            content: input.content,
            attachments: stripAttachmentPreviews(input.attachments ?? []),
            client_message_key: input.clientMessageKey,
          },
        },
      );
      return payload.message;
    },
    async updateSessionMessage(channelId, sessionId, messageId, input) {
      const payload = await request<{ message: ChannelMessage }>(
        `/api/channels/${channelId}/sessions/${sessionId}/messages/${messageId}`,
        {
          method: "PUT",
          body: {
            content: input.content,
            attachments: stripAttachmentPreviews(input.attachments ?? []),
          },
        },
      );
      return payload.message;
    },
    async uploadSessionAttachments(channelId, sessionId, files) {
      const form = new FormData();
      for (const file of files) {
        form.append("file", file, file.name);
      }

      const response = await fetch(
        `/api/channels/${channelId}/sessions/${sessionId}/attachments`,
        {
          method: "POST",
          credentials: "include",
          body: form,
        },
      );

      if (!response.ok) {
        const message = await response
          .json()
          .then((value) => value.message ?? value.error ?? response.statusText)
          .catch(() => response.statusText);
        throw new Error(String(message));
      }

      const payload = (await response.json()) as { attachments: HermesAttachment[] };
      return payload.attachments;
    },
    async createChannelRun(channelId, sessionId, input) {
      return request<{ message: ChannelMessage; run: ChannelRun }>(
        `/api/channels/${channelId}/sessions/${sessionId}/runs`,
        {
          method: "POST",
          body: {
            content: input.content,
            attachments: stripAttachmentPreviews(input.attachments ?? []),
            client_message_key: input.clientMessageKey,
          },
        },
      );
    },
    async generateSessionTitle(channelId, sessionId, prompt) {
      const payload = await request<{ session: ChannelSession }>(
        `/api/channels/${channelId}/sessions/${sessionId}/title`,
        { method: "POST", body: { prompt } },
      );
      return payload.session;
    },
    async workspaceStatus() {
      const payload = await request<{ hermes_instance: HermesInstance | null }>(
        "/api/workspace/status",
      );
      return payload.hermes_instance;
    },
    async ensureHermes() {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        "/api/workspace/ensure-hermes",
        { method: "POST" },
      );
      return payload.hermes_instance;
    },
    async modelConfig() {
      const payload = await this.modelConfigStatus();
      return payload.model_config;
    },
    async modelConfigStatus() {
      const payload = await request<ModelConfigStatus>(
        "/api/admin/model-config",
      );
      const modelConfigs = payload.model_configs ?? [payload.model_config];
      return {
        ...payload,
        model_config: payload.model_config,
        model_configs: modelConfigs,
        required_models_ready: payload.required_models_ready ?? false,
        missing_required_model_config_kinds:
          payload.missing_required_model_config_kinds ?? [],
      };
    },
    async modelConfigs() {
      const payload = await this.modelConfigStatus();
      return payload.model_configs;
    },
    async updateModelConfig(config) {
      await updateModelConfigRequest(config);
    },
    async updateModelConfigs(configs) {
      // 当前后端逐类保存模型配置；前端只暴露一个提交动作，避免管理员漏保存某一类。
      for (const config of configs) {
        await updateModelConfigRequest(config);
      }
    },
    async testModelConfig(config) {
      return request<ModelConfigTestResult>(
        `/api/admin/model-config/${config.config_kind}/test`,
        {
          method: "POST",
          body: normalizedModelConfig(config),
        },
      );
    },
    async systemSettings() {
      const payload = await request<{ settings: SystemSettings }>(
        "/api/admin/system-settings",
      );
      return payload.settings;
    },
    async updateSystemSettings(settings) {
      await request<void>("/api/admin/system-settings", {
        method: "PUT",
        body: settings,
      });
    },
    async sendHermesPrompt(prompt, attachments = [], sessionId, handlers) {
      const response = await fetch("/api/hermes/v1/runs", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          input: hermesRunInput(prompt, attachments),
          stream: true,
          session_id: sessionId,
        }),
      });

      if (!response.ok) {
        throw new Error(`Hermes request failed: ${response.status}`);
      }

      if (response.status === 202) {
        const started = (await response.json()) as HermesRunStarted;
        if (!started.run_id) {
          throw new Error("Hermes run did not return a run_id");
        }
        handlers?.onRunStarted?.(started.run_id);
        return readHermesRunEvents(started.run_id, handlers);
      }

      const text = await response.text();
      handlers?.onOutput?.(text);
      return text;
    },
    async activeHermesRun(channelId, sessionId) {
      const payload = await request<{ active_run: HermesActiveRun | null }>(
        `/api/channels/${channelId}/sessions/${sessionId}/active-run`,
      );
      return payload.active_run;
    },
    subscribeSessionEvents(channelId, sessionId, onEvent, onError) {
      const source = new EventSource(
        `/api/channels/${channelId}/sessions/${sessionId}/events`,
        { withCredentials: true },
      );
      const eventNames = [
        "messages_snapshot",
        "message_created",
        "message_updated",
        "run_updated",
        "run_cleared",
        "session_deleted",
      ];
      const listeners = eventNames.map((eventName) => {
        const listener = (event: MessageEvent) => {
          try {
            onEvent(JSON.parse(event.data) as ChannelSessionEvent);
          } catch (cause) {
            onError?.(cause instanceof Error ? cause : new Error("invalid session event"));
          }
        };
        source.addEventListener(eventName, listener);
        return [eventName, listener] as const;
      });
      source.onerror = () => {
        // EventSource 会自动重连；这里仅上报给测试/诊断，不在 UI 上显示 load failed。
        onError?.(new Error("session event stream disconnected"));
      };
      return () => {
        for (const [eventName, listener] of listeners) {
          source.removeEventListener(eventName, listener);
        }
        source.close();
      };
    },
    async stopHermesRun(channelId, sessionId) {
      const payload = await request<{ active_run: HermesActiveRun | null }>(
        `/api/channels/${channelId}/sessions/${sessionId}/active-run/stop`,
        { method: "POST" },
      );
      return payload.active_run;
    },
    async clearHermesRun(channelId, sessionId) {
      await request<void>(`/api/channels/${channelId}/sessions/${sessionId}/active-run`, {
        method: "DELETE",
      });
    },
    async resumeHermesRun(runId, handlers) {
      return readHermesRunEvents(runId, handlers);
    },
  };
}

function hermesRunInput(prompt: string, attachments: HermesAttachment[]) {
  if (attachments.length === 0) {
    return prompt;
  }

  const content = [];
  if (prompt.trim()) {
    content.push({ type: "text", text: prompt.trim() });
  }

  for (const attachment of attachments) {
    const url = attachment.data_url ?? absoluteAttachmentUrl(attachment.download_url);
    if (attachment.kind === "image" && url) {
      content.push({
        type: "image_url",
        image_url: { url },
      });
    } else {
      // Hermes runs 端点当前没有稳定的通用文件上传字段；先把 Hub 附件引用并入文本上下文。
      content.push({
        type: "text",
        text: `[Attached file: ${attachment.name} (${attachment.content_type})${
          attachment.download_url ? ` ${attachment.download_url}` : ""
        }]`,
      });
    }
  }

  return [{ role: "user", content }];
}

function absoluteAttachmentUrl(url: string | undefined) {
  if (!url) {
    return undefined;
  }
  if (/^https?:\/\//i.test(url) || url.startsWith("data:")) {
    return url;
  }
  return `${globalThis.location?.origin ?? ""}${url}`;
}

function stripAttachmentPreviews(attachments: HermesAttachment[]): HermesAttachment[] {
  return attachments.map(({ data_url: _dataUrl, ...attachment }) => attachment);
}

async function readHermesRunEvents(
  runId: string,
  handlers?: HermesStreamHandlers,
): Promise<string> {
  const progress: HermesStreamProgress = {
    receivedBytes: 0,
    deltaText: "",
    completedOutput: "",
    pendingEventName: "",
    pendingDataLines: [],
  };
  let lastError: unknown = null;

  for (let attempt = 0; attempt < 8; attempt += 1) {
    try {
      return await readHermesRunEventsOnce(runId, handlers, progress);
    } catch (cause) {
      if (!isReconnectableHermesStreamError(cause)) {
        throw cause;
      }

      lastError = cause;
      await waitForReconnectDelay(attempt);
    }
  }

  if (progress.completedOutput || progress.deltaText.trim()) {
    handlers?.onOutput?.(progress.completedOutput || progress.deltaText);
    return progress.completedOutput || progress.deltaText;
  }

  throw lastError instanceof Error ? lastError : new Error("Hermes stream interrupted");
}

async function readHermesRunEventsOnce(
  runId: string,
  handlers: HermesStreamHandlers | undefined,
  progress: HermesStreamProgress,
): Promise<string> {
  const headers: Record<string, string> = { Accept: "text/event-stream" };
  if (progress.receivedBytes > 0) {
    headers["X-Hermes-Hub-Received-Bytes"] = String(progress.receivedBytes);
  }

  const response = await fetch(`/api/hermes/v1/runs/${encodeURIComponent(runId)}/events`, {
    method: "GET",
    credentials: "include",
    headers,
  });

  if (!response.ok) {
    throw new Error(`Hermes run events failed: ${response.status}`);
  }

  if (response.body) {
    return readHermesRunEventsFromBody(response.body, handlers, progress);
  }

  const eventStream = await response.text();
  progress.receivedBytes += new TextEncoder().encode(eventStream).byteLength;
  const events = parseHermesRunEventLines(eventStream.split(/\r?\n/), handlers, progress);
  events.push(...flushHermesRunEventParser(handlers, progress));
  const result = reduceHermesRunEvents(events);
  progress.deltaText += result.deltaText;
  if (result.completedOutput) {
    progress.completedOutput = result.completedOutput;
    handlers?.onOutput?.(result.completedOutput);
  }
  return progress.completedOutput || progress.deltaText || eventStream;
}

async function readHermesRunEventsFromBody(
  body: ReadableStream<Uint8Array>,
  handlers?: HermesStreamHandlers,
  progress: HermesStreamProgress = {
    receivedBytes: 0,
    deltaText: "",
    completedOutput: "",
    pendingEventName: "",
    pendingDataLines: [],
  },
): Promise<string> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let deltaText = "";
  let completedOutput = "";

  try {
    while (true) {
      let chunk: ReadableStreamReadResult<Uint8Array>;
      try {
        chunk = await reader.read();
      } catch (cause) {
        if (completedOutput || progress.completedOutput) {
          const recovered = completedOutput || progress.completedOutput;
          handlers?.onOutput?.(recovered);
          return recovered;
        }

        // 前端到 Hub 的 SSE 可能因为切页、移动端后台、网络切换而中断；
        // 这里抛出可重连错误，外层会用已收到字节数恢复同一个 Hermes run。
        throw new Error(cause instanceof Error ? cause.message : "Hermes stream interrupted");
      }

      if (chunk.done) {
        break;
      }

      progress.receivedBytes += chunk.value.byteLength;
      buffer += decoder.decode(chunk.value, { stream: true });
      const lines = buffer.split(/\r?\n/);
      buffer = lines.pop() ?? "";
      const result = reduceHermesRunEvents(parseHermesRunEventLines(lines, handlers, progress));
      deltaText += result.deltaText;
      progress.deltaText += result.deltaText;
      if (result.completedOutput) {
        completedOutput = result.completedOutput;
        progress.completedOutput = result.completedOutput;
      }
    }
  } finally {
    reader.releaseLock();
  }

  buffer += decoder.decode();
  if (buffer) {
    const result = reduceHermesRunEvents(
      parseHermesRunEventLines(buffer.split(/\r?\n/), handlers, progress),
    );
    deltaText += result.deltaText;
    progress.deltaText += result.deltaText;
    if (result.completedOutput) {
      completedOutput = result.completedOutput;
      progress.completedOutput = result.completedOutput;
    }
  }
  const flushed = reduceHermesRunEvents(flushHermesRunEventParser(handlers, progress));
  deltaText += flushed.deltaText;
  progress.deltaText += flushed.deltaText;
  if (flushed.completedOutput) {
    completedOutput = flushed.completedOutput;
    progress.completedOutput = flushed.completedOutput;
  }

  if (completedOutput) {
    handlers?.onOutput?.(completedOutput);
  }
  return progress.completedOutput || progress.deltaText || deltaText;
}

function reduceHermesRunEvents(events: HermesRunEvent[]) {
  let deltaText = "";
  let completedOutput = "";

  for (const event of events) {
    if (event.event === "message.delta" && event.delta) {
      deltaText += event.delta;
    }
    if (event.event === "run.completed" && event.output) {
      completedOutput = event.output;
    }
    if (event.event === "run.failed") {
      throw new HermesRunFailedError(errorMessageFromEvent(event.error) || "Hermes run failed");
    }
  }

  return { deltaText, completedOutput };
}

function isReconnectableHermesStreamError(cause: unknown) {
  if (cause instanceof HermesRunFailedError) {
    return false;
  }

  const message = cause instanceof Error ? cause.message : String(cause);
  return (
    /load failed|network|interrupted|terminated|aborted|body stream/i.test(message) ||
    /Hermes run events failed: (408|429|500|502|503|504)/.test(message)
  );
}

function waitForReconnectDelay(attempt: number) {
  const delayMs = Math.min(1500, 150 * 2 ** attempt);
  return new Promise((resolve) => globalThis.setTimeout(resolve, delayMs));
}

function parseHermesRunEventLines(
  lines: string[],
  handlers?: HermesStreamHandlers,
  progress: HermesStreamProgress = {
    receivedBytes: 0,
    deltaText: "",
    completedOutput: "",
    pendingEventName: "",
    pendingDataLines: [],
  },
): HermesRunEvent[] {
  const events: HermesRunEvent[] = [];

  for (const line of lines) {
    if (line.startsWith("event:")) {
      const previous = parseHermesRunEvent(progress.pendingEventName, progress.pendingDataLines);
      if (previous) {
        notifyHermesEventHandlers(previous, handlers);
        events.push(previous);
        progress.pendingDataLines = [];
      }
      progress.pendingEventName = line.slice("event:".length).trim();
      continue;
    }
    if (line.startsWith("data:")) {
      if (!progress.pendingEventName && progress.pendingDataLines.length > 0) {
        const previous = parseHermesRunEvent(progress.pendingEventName, progress.pendingDataLines);
        if (previous) {
          notifyHermesEventHandlers(previous, handlers);
          events.push(previous);
        }
        progress.pendingDataLines = [];
      }
      progress.pendingDataLines.push(line.slice("data:".length).trim());
      continue;
    }
    if (line.trim() === "") {
      const event = parseHermesRunEvent(progress.pendingEventName, progress.pendingDataLines);
      progress.pendingEventName = "";
      progress.pendingDataLines = [];
      if (event) {
        notifyHermesEventHandlers(event, handlers);
        events.push(event);
      }
    }
  }

  return events;
}

function flushHermesRunEventParser(
  handlers: HermesStreamHandlers | undefined,
  progress: HermesStreamProgress,
) {
  const event = parseHermesRunEvent(progress.pendingEventName, progress.pendingDataLines);
  progress.pendingEventName = "";
  progress.pendingDataLines = [];
  if (!event) {
    return [];
  }

  notifyHermesEventHandlers(event, handlers);
  return [event];
}

function parseHermesRunEvent(eventName: string, dataLines: string[]): HermesRunEvent | null {
  if (dataLines.length === 0) {
    return null;
  }

  const data = dataLines.join("\n").trim();
  if (!data || data === "[DONE]") {
    return null;
  }

  try {
    const event = JSON.parse(data) as HermesRunEvent;
    return {
      ...event,
      event: event.event ?? eventName,
    };
  } catch {
    return eventName ? { event: eventName, message: data } : null;
  }
}

function notifyHermesEventHandlers(event: HermesRunEvent, handlers?: HermesStreamHandlers) {
  if (event.event === "message.delta" && event.delta) {
    handlers?.onDelta?.(event.delta);
  }

  const verbose = verboseMessageFromHermesEvent(event);
  if (verbose) {
    handlers?.onVerbose?.(verbose);
  }
}

function verboseMessageFromHermesEvent(event: HermesRunEvent): HermesVerboseEvent | null {
  if (event.message?.trim()) {
    return { kind: "text", detail: normalizeVerboseText(event.message) ?? event.message };
  }
  const item = event.item;

  if (event.event === "approval.request") {
    return {
      kind: "approval.request",
      detail:
        firstVerboseDetail(
          event.command,
          event.preview,
          event.description,
          event.output,
          item?.arguments,
          item?.input,
          item?.output,
          item?.content,
        ) ?? undefined,
    };
  }

  if (event.event === "approval.responded") {
    return {
      kind: "approval.responded",
      choice: event.choice ?? "session",
    };
  }

  if (event.event === "reasoning.available") {
    return {
      kind: "text",
      detail: firstVerboseDetail(event.text, event.preview, event.message) ?? undefined,
    };
  }

  if (event.event === "tool.started") {
    return verboseToolEvent(
      "tool.started",
      event,
      firstVerboseDetail(
        event.preview,
        event.command,
        event.description,
        item?.arguments,
        item?.input,
        item?.output,
        item?.content,
      ),
    );
  }

  if (event.event === "tool.completed") {
    return verboseToolEvent(
      "tool.completed",
      event,
      firstVerboseDetail(
        errorMessageFromEvent(event.error),
        event.output,
        item?.output,
        item?.content,
        event.preview,
        event.command,
        event.description,
        item?.arguments,
        item?.input,
        durationDetail(event.duration),
      ),
      Boolean(event.error),
    );
  }

  if (event.event === "hermes.tool.progress") {
    const detail = firstVerboseDetail(
      event.preview,
      event.command,
      event.description,
      event.output,
      item?.output,
      item?.content,
    );
    const status = event.status ?? "";
    return verboseToolEvent(
      status === "completed" || status === "done" ? "tool.completed" : "tool.progress",
      event,
      detail,
    );
  }

  if (
    event.item &&
    (event.event === "response.output_item.added" || event.event === "response.output_item.done") &&
    (event.item.type === "function_call" || event.item.type === "function_call_output")
  ) {
    const item = event.item;
    return {
      kind: event.event === "response.output_item.done" ? "tool.completed" : "tool.call",
      tool: item.name ?? undefined,
      detail:
        firstVerboseDetail(
          item.arguments,
          item.input,
          item.output,
          item.content,
          event.preview,
          event.command,
          event.description,
          event.output,
        ) ?? undefined,
    };
  }

  if (event.event?.includes("tool")) {
    return verboseToolEvent(
      event.event.includes("completed") || event.event.includes("done")
        ? "tool.completed"
        : "tool.started",
      event,
      firstVerboseDetail(
        event.preview,
        event.command,
        event.description,
        event.output,
        item?.output,
        item?.content,
        item?.arguments,
        item?.input,
      ),
    );
  }

  return null;
}

function verboseToolEvent(
  kind: HermesVerboseEvent["kind"],
  event: HermesRunEvent,
  detail?: string | null,
  failed = false,
): HermesVerboseEvent {
  return {
    kind,
    tool: event.tool ?? event.name ?? event.item?.name ?? undefined,
    detail: detail ?? undefined,
    failed,
  };
}

function firstVerboseDetail(...values: Array<string | null | undefined>) {
  for (const value of values) {
    const normalized = normalizeVerboseText(value);
    if (normalized) {
      return normalized;
    }
  }
  return null;
}

function normalizeVerboseText(value: string | null | undefined) {
  return value?.replace(/\s+/g, " ").trim() || null;
}

function errorMessageFromEvent(error: HermesRunEvent["error"]) {
  if (typeof error === "string") {
    return normalizeVerboseText(error);
  }
  if (typeof error === "object" && error?.message) {
    return normalizeVerboseText(error.message);
  }
  return null;
}

function durationDetail(duration: number | undefined) {
  return typeof duration === "number" ? `${duration.toFixed(3)}s` : null;
}

type MockApiClientOptions = {
  initialUser?: User | null;
  oidcPublicConfig?: OidcPublicConfig;
  bootstrapOpen?: boolean;
  requiredModelsReady?: boolean;
  missingRequiredModelConfigKinds?: ModelConfigKind[];
  initialInstance?: HermesInstance | null;
  initialMessagesBySessionId?: Record<string, ChannelMessage[]>;
  createChannelRun?: ApiClient["createChannelRun"];
  sendHermesPrompt?: ApiClient["sendHermesPrompt"];
  activeRunsBySessionId?: Record<string, HermesActiveRun>;
  subscribeSessionEvents?: ApiClient["subscribeSessionEvents"];
  resumeHermesRun?: ApiClient["resumeHermesRun"];
  stopHermesRun?: ApiClient["stopHermesRun"];
  deleteSession?: ApiClient["deleteSession"];
  createSession?: ApiClient["createSession"];
};

export function createMockApiClient(options: MockApiClientOptions = {}): ApiClient {
  let hasAnyUser = options.bootstrapOpen === true ? false : true;
  let currentUser: User | null = "initialUser" in options ? options.initialUser! : {
    id: "user-1",
    email: "admin@example.com",
    role: "admin",
    status: "active",
  };
  let channels: Channel[] = [
    {
      id: "channel-1",
      name: "hermes-hub",
      description: "Hermes Hub default channel",
    },
  ];
  let sessions: ChannelSession[] = [
    {
      id: "session-1",
      channel_id: "channel-1",
      kind: "agent",
      title: "Session",
      created_at: Date.now(),
      updated_at: Date.now(),
    },
  ];
  let messagesBySessionId: Record<string, ChannelMessage[]> = {
    "session-1": [],
    ...(options.initialMessagesBySessionId ?? {}),
  };
  let activeRunsBySessionId = { ...(options.activeRunsBySessionId ?? {}) };
  const sessionEventListenersBySessionId: Record<
    string,
    Set<(event: ChannelSessionEvent) => void>
  > = {};
  let invites: Invite[] = [];
  let instance: HermesInstance | null = "initialInstance" in options ? options.initialInstance! : {
    id: "instance-1",
    user_id: "user-1",
    kind: "managed_docker",
    status: "running",
    base_url: "http://hermes-user-user-1:8000",
  };
  let modelConfig: ModelConfig = {
    config_kind: "llm",
    provider_name: "openai-compatible",
    provider_base_url: "https://ready-provider.example/v1",
    provider_api_key: "ready-provider-key",
    default_model: "gpt-4.1-mini",
    allowed_models: ["gpt-4.1-mini"],
    api_type: "chat_completions",
    reasoning_effort: null,
    allow_streaming: true,
    request_timeout_seconds: 60,
  };
  let modelConfigs: ModelConfig[] = [
    modelConfig,
    {
      ...modelConfig,
      config_kind: "image",
      default_model: "gpt-image-1",
      allowed_models: ["gpt-image-1"],
      api_type: "images_generations",
      reasoning_effort: null,
      allow_streaming: false,
    },
    {
      ...modelConfig,
      config_kind: "title",
      api_type: "chat_completions",
      allow_streaming: false,
    },
  ];
  let systemSettings: SystemSettings = {
    max_sessions_per_user: 20,
    oidc: defaultOidcSettings(),
  };

  function emitSessionEvent(sessionId: string, event: ChannelSessionEvent) {
    for (const listener of sessionEventListenersBySessionId[sessionId] ?? []) {
      listener(event);
    }
  }

  return {
    async me() {
      return currentUser;
    },
    async bootstrapStatus() {
      return { bootstrap_open: !hasAnyUser };
    },
    async oidcConfig() {
      return options.oidcPublicConfig ?? {
        enabled: systemSettings.oidc.enabled,
        display_name: systemSettings.oidc.display_name,
      };
    },
    async login(email) {
      hasAnyUser = true;
      currentUser = { id: "user-1", email, role: "admin", status: "active" };
      return currentUser;
    },
    async bootstrapRegister(email) {
      hasAnyUser = true;
      currentUser = { id: "user-1", email, role: "admin", status: "active" };
      return currentUser;
    },
    async registerWithInvite(_inviteToken, email) {
      hasAnyUser = true;
      currentUser = { id: "user-2", email, role: "user", status: "active" };
      return currentUser;
    },
    async logout() {
      currentUser = null;
    },
    async listUsers() {
      return currentUser ? [currentUser] : [];
    },
    async disableUser(userId) {
      currentUser = { ...(currentUser as User), id: userId, status: "disabled" };
      return currentUser;
    },
    async enableUser(userId) {
      currentUser = { ...(currentUser as User), id: userId, status: "active" };
      return currentUser;
    },
    async listInvites() {
      return invites;
    },
    async createInvite(input) {
      const invite: Invite = {
        id: `invite-${invites.length + 1}`,
        status: "pending",
        expires_at: input.expires_at,
        max_uses: input.max_uses,
        used_count: 0,
      };
      invites = [invite, ...invites];
      return { token: "mock-invite-token", invite };
    },
    async revokeInvite(inviteId) {
      invites = invites.map((invite) =>
        invite.id === inviteId ? { ...invite, status: "revoked" } : invite,
      );
      return invites.find((invite) => invite.id === inviteId)!;
    },
    async listHermesInstances() {
      return instance ? [instance] : [];
    },
    async createHermesInstance(userId) {
      instance = {
        id: "instance-1",
        user_id: userId,
        kind: "managed_docker",
        status: "running",
        base_url: `http://hermes-user-${userId}:8000`,
      };
      return instance;
    },
    async startHermesInstance() {
      instance = { ...(instance as HermesInstance), status: "running" };
      return instance;
    },
    async stopHermesInstance() {
      instance = { ...(instance as HermesInstance), status: "stopped" };
      return instance;
    },
    async rebuildHermesInstance() {
      instance = { ...(instance as HermesInstance), status: "running" };
      return instance;
    },
    async updateExternalHermesInstanceConfig(_userId, input) {
      instance = {
        ...(instance as HermesInstance),
        kind: "external",
        name: input.name,
        base_url: input.base_url,
      };
      return instance;
    },
    async listChannels() {
      return channels;
    },
    async createChannel(name, description) {
      const channel = { id: `channel-${channels.length + 1}`, name, description };
      channels = [channel, ...channels];
      return channel;
    },
    async listSessions(channelId) {
      return sessions.filter((session) => session.channel_id === channelId);
    },
    async createSession(channelId, kind, title) {
      if (options.createSession) {
        return options.createSession(channelId, kind, title);
      }

      const now = Date.now();
      const session = {
        id: `session-${sessions.length + 1}`,
        channel_id: channelId,
        kind,
        title,
        created_at: now,
        updated_at: now,
      };
      sessions = [session, ...sessions];
      messagesBySessionId[session.id] = [];
      return session;
    },
    async deleteSession(channelId, sessionId) {
      if (options.deleteSession) {
        await options.deleteSession(channelId, sessionId);
      }
      sessions = sessions.filter(
        (session) => !(session.channel_id === channelId && session.id === sessionId),
      );
      delete messagesBySessionId[sessionId];
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, { type: "session_deleted", session_id: sessionId });
    },
    async listSessionMessages(_channelId, sessionId) {
      return messagesBySessionId[sessionId] ?? [];
    },
    async appendSessionMessage(_channelId, sessionId, input) {
      const existing = input.clientMessageKey
        ? (messagesBySessionId[sessionId] ?? []).find(
            (message) => message.client_message_key === input.clientMessageKey,
          )
        : undefined;
      if (existing) {
        return existing;
      }

      const message: ChannelMessage = {
        id: `message-${(messagesBySessionId[sessionId] ?? []).length + 1}`,
        session_id: sessionId,
        role: input.role,
        client_message_key: input.clientMessageKey,
        content: input.content,
        attachments: input.attachments ?? [],
        created_at: Date.now(),
      };
      messagesBySessionId[sessionId] = [...(messagesBySessionId[sessionId] ?? []), message];
      sessions = sessions.map((session) =>
        session.id === sessionId ? { ...session, updated_at: Date.now() } : session,
      );
      emitSessionEvent(sessionId, { type: "message_created", message });
      return message;
    },
    async updateSessionMessage(_channelId, sessionId, messageId, input) {
      const messages = messagesBySessionId[sessionId] ?? [];
      const existing = messages.find((message) => message.id === messageId);
      if (!existing) {
        throw new Error("message not found");
      }
      const nextMessage = {
        ...existing,
        content: input.content,
        attachments: input.attachments ?? [],
      };
      messagesBySessionId[sessionId] = messages.map((message) =>
        message.id === messageId ? nextMessage : message,
      );
      sessions = sessions.map((session) =>
        session.id === sessionId ? { ...session, updated_at: Date.now() } : session,
      );
      emitSessionEvent(sessionId, { type: "message_updated", message: nextMessage });
      return nextMessage;
    },
    async uploadSessionAttachments(_channelId, sessionId, files) {
      return files.map((file, index) => ({
        id: `attachment-${sessionId}-${index + 1}`,
        name: file.name,
        content_type: file.type || "application/octet-stream",
        kind: file.type.startsWith("image/") ? "image" : "file",
        size: file.size,
        download_url: `/api/attachments/attachment-${sessionId}-${index + 1}/download`,
      }));
    },
    async createChannelRun(_channelId, sessionId, input) {
      if (options.createChannelRun) {
        return options.createChannelRun(_channelId, sessionId, input);
      }

      const message = await this.appendSessionMessage("channel-1", sessionId, {
        role: "user",
        content: input.content,
        attachments: input.attachments ?? [],
        clientMessageKey: input.clientMessageKey,
      });
      const run: ChannelRun = {
        id: `run-${Date.now()}`,
        run_id: `hub-run-${Date.now()}`,
        session_id: sessionId,
        user_message_id: message.id,
        status: "queued",
        input: input.content,
        input_attachments: input.attachments ?? [],
        created_at: Date.now(),
        updated_at: Date.now(),
      };
      activeRunsBySessionId[sessionId] = {
        run_id: run.run_id,
        status: run.status,
        created_at: run.created_at,
        updated_at: run.updated_at,
      };
      emitSessionEvent(sessionId, { type: "run_updated", run });

      await Promise.resolve();
      const assistant: ChannelMessage = {
        id: `message-${(messagesBySessionId[sessionId] ?? []).length + 1}`,
        session_id: sessionId,
        role: "assistant",
        client_message_key: `hermes-run:${run.run_id}`,
        content: input.content,
        attachments: [],
        created_at: Date.now(),
      };
      messagesBySessionId[sessionId] = [...(messagesBySessionId[sessionId] ?? []), assistant];
      emitSessionEvent(sessionId, { type: "message_created", message: assistant });
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, { type: "run_cleared", session_id: sessionId });
      return { message, run };
    },
    async generateSessionTitle(channelId, sessionId, prompt) {
      const session = sessions.find((item) => item.id === sessionId && item.channel_id === channelId);
      if (!session) {
        throw new Error("session not found");
      }
      const titled = {
        ...session,
        title: prompt.trim().slice(0, 48) || "New conversation",
      };
      sessions = sessions.map((item) => (item.id === sessionId ? titled : item));
      return titled;
    },
    async workspaceStatus() {
      return instance;
    },
    async ensureHermes() {
      instance = instance ?? {
        id: "instance-1",
        user_id: "user-1",
        kind: "managed_docker",
        status: "running",
        base_url: "http://hermes-user-user-1:8000",
      };
      return instance;
    },
    async modelConfig() {
      return modelConfig;
    },
    async modelConfigStatus() {
      return {
        model_config: modelConfig,
        model_configs: modelConfigs,
        required_models_ready: options.requiredModelsReady ?? true,
        missing_required_model_config_kinds:
          options.missingRequiredModelConfigKinds ?? [],
      };
    },
    async modelConfigs() {
      return modelConfigs;
    },
    async updateModelConfig(config) {
      const normalized = normalizedModelConfig(config);
      modelConfigs = modelConfigs.map((existing) =>
        existing.config_kind === config.config_kind
          ? normalized
          : existing,
      );
      modelConfig =
        modelConfigs.find((existing) => existing.config_kind === "llm") ?? modelConfig;
    },
    async updateModelConfigs(configs) {
      for (const config of configs) {
        await this.updateModelConfig(config);
      }
    },
    async testModelConfig() {
      return {
        ok: true,
        status_code: 200,
        message: "model test succeeded",
        duration_ms: 12,
      };
    },
    async systemSettings() {
      return systemSettings;
    },
    async updateSystemSettings(settings) {
      systemSettings = settings;
    },
    async sendHermesPrompt(prompt, _attachments, _sessionId, handlers) {
      if (options.sendHermesPrompt) {
        return options.sendHermesPrompt(prompt, _attachments, _sessionId, handlers);
      }

      // 让 mock 行为接近真实 fetch 流：delta 会在调用栈释放后到达。
      await Promise.resolve();
      handlers?.onDelta?.(prompt);
      return prompt;
    },
    async activeHermesRun(_channelId, sessionId) {
      return activeRunsBySessionId[sessionId] ?? null;
    },
    subscribeSessionEvents(channelId, sessionId, onEvent, onError) {
      if (options.subscribeSessionEvents) {
        return options.subscribeSessionEvents(channelId, sessionId, onEvent, onError);
      }
      sessionEventListenersBySessionId[sessionId] =
        sessionEventListenersBySessionId[sessionId] ?? new Set();
      sessionEventListenersBySessionId[sessionId].add(onEvent);
      queueMicrotask(() => {
        onEvent({
          type: "messages_snapshot",
          messages: messagesBySessionId[sessionId] ?? [],
          active_run: activeRunsBySessionId[sessionId] ?? null,
        });
      });
      return () => {
        sessionEventListenersBySessionId[sessionId]?.delete(onEvent);
      };
    },
    async stopHermesRun(channelId, sessionId) {
      if (options.stopHermesRun) {
        return options.stopHermesRun(channelId, sessionId);
      }
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, { type: "run_cleared", session_id: sessionId });
      return null;
    },
    async clearHermesRun(_channelId, sessionId) {
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, { type: "run_cleared", session_id: sessionId });
    },
    async resumeHermesRun(runId, handlers) {
      if (options.resumeHermesRun) {
        return options.resumeHermesRun(runId, handlers);
      }
      await Promise.resolve();
      handlers?.onDelta?.("");
      return "";
    },
  };
}
