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
};

export type ChannelMessage = {
  id: string;
  session_id: string;
  role: "user" | "assistant";
  content: string;
  attachments: HermesAttachment[];
  created_at: number;
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

export type HermesAttachment = {
  id?: string;
  name: string;
  content_type: string;
  kind: "file" | "image";
  size?: number;
  download_url?: string;
  data_url?: string;
};

type HermesRunStarted = {
  run_id?: string;
  status?: string;
};

type HermesRunEvent = {
  event?: string;
  delta?: string;
  output?: string;
  error?: string;
};

export type HermesStreamHandlers = {
  onDelta?: (delta: string) => void;
  onOutput?: (output: string) => void;
};

export type CreateInviteInput = {
  expires_at: number;
  max_uses: number;
};

export type ApiClient = {
  me: () => Promise<User | null>;
  bootstrapStatus: () => Promise<{ bootstrap_open: boolean }>;
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
  listSessionMessages: (channelId: string, sessionId: string) => Promise<ChannelMessage[]>;
  appendSessionMessage: (
    channelId: string,
    sessionId: string,
    input: {
      role: "user" | "assistant";
      content: string;
      attachments?: HermesAttachment[];
    },
  ) => Promise<ChannelMessage>;
  uploadSessionAttachments: (
    channelId: string,
    sessionId: string,
    files: File[],
  ) => Promise<HermesAttachment[]>;
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
  sendHermesPrompt: (
    prompt: string,
    attachments?: HermesAttachment[],
    sessionId?: string,
    handlers?: HermesStreamHandlers,
  ) => Promise<string>;
};

type RequestOptions = {
  method?: "GET" | "POST" | "PUT";
  body?: unknown;
  allowUnauthorized?: boolean;
};

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
    const message = await response
      .json()
      .then((value) => value.message ?? value.error ?? response.statusText)
      .catch(() => response.statusText);
    throw new Error(String(message));
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
        return readHermesRunEvents(started.run_id, handlers);
      }

      const text = await response.text();
      handlers?.onOutput?.(text);
      return text;
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
  const response = await fetch(`/api/hermes/v1/runs/${encodeURIComponent(runId)}/events`, {
    method: "GET",
    credentials: "include",
    headers: { Accept: "text/event-stream" },
  });

  if (!response.ok) {
    throw new Error(`Hermes run events failed: ${response.status}`);
  }

  if (response.body) {
    return readHermesRunEventsFromBody(response.body, handlers);
  }

  const eventStream = await response.text();
  const events = parseHermesRunEventLines(eventStream.split(/\r?\n/), handlers);
  const result = reduceHermesRunEvents(events);
  if (result.completedOutput) {
    handlers?.onOutput?.(result.completedOutput);
  }
  return result.completedOutput || result.deltaText || eventStream;
}

async function readHermesRunEventsFromBody(
  body: ReadableStream<Uint8Array>,
  handlers?: HermesStreamHandlers,
): Promise<string> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let deltaText = "";
  let completedOutput = "";

  while (true) {
    const { done, value } = await reader.read();
    if (done) {
      break;
    }

    buffer += decoder.decode(value, { stream: true });
    const lines = buffer.split(/\r?\n/);
    buffer = lines.pop() ?? "";
    const result = reduceHermesRunEvents(parseHermesRunEventLines(lines, handlers));
    deltaText += result.deltaText;
    if (result.completedOutput) {
      completedOutput = result.completedOutput;
    }
  }

  buffer += decoder.decode();
  if (buffer) {
    const result = reduceHermesRunEvents(parseHermesRunEventLines(buffer.split(/\r?\n/), handlers));
    deltaText += result.deltaText;
    if (result.completedOutput) {
      completedOutput = result.completedOutput;
    }
  }

  if (completedOutput) {
    handlers?.onOutput?.(completedOutput);
  }
  return completedOutput || deltaText;
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
      throw new Error(event.error || "Hermes run failed");
    }
  }

  return { deltaText, completedOutput };
}

function parseHermesRunEventLines(
  lines: string[],
  handlers?: HermesStreamHandlers,
): HermesRunEvent[] {
  return lines
    .filter((line) => line.startsWith("data: "))
    .map((line) => line.slice("data: ".length).trim())
    .filter(Boolean)
    .flatMap((line) => {
      try {
        const event = JSON.parse(line) as HermesRunEvent;
        if (event.event === "message.delta" && event.delta) {
          handlers?.onDelta?.(event.delta);
        }
        return [event];
      } catch {
        return [];
      }
    });
}

type MockApiClientOptions = {
  initialUser?: User | null;
  bootstrapOpen?: boolean;
  requiredModelsReady?: boolean;
  missingRequiredModelConfigKinds?: ModelConfigKind[];
  initialInstance?: HermesInstance | null;
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
  let sessions: ChannelSession[] = [{ id: "session-1", channel_id: "channel-1", kind: "agent", title: "Session" }];
  let messagesBySessionId: Record<string, ChannelMessage[]> = {
    "session-1": [],
  };
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

  return {
    async me() {
      return currentUser;
    },
    async bootstrapStatus() {
      return { bootstrap_open: !hasAnyUser };
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
      const session = { id: `session-${sessions.length + 1}`, channel_id: channelId, kind, title };
      sessions = [session, ...sessions];
      messagesBySessionId[session.id] = [];
      return session;
    },
    async listSessionMessages(_channelId, sessionId) {
      return messagesBySessionId[sessionId] ?? [];
    },
    async appendSessionMessage(_channelId, sessionId, input) {
      const message: ChannelMessage = {
        id: `message-${(messagesBySessionId[sessionId] ?? []).length + 1}`,
        session_id: sessionId,
        role: input.role,
        content: input.content,
        attachments: input.attachments ?? [],
        created_at: Date.now(),
      };
      messagesBySessionId[sessionId] = [...(messagesBySessionId[sessionId] ?? []), message];
      return message;
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
    async sendHermesPrompt(prompt, _attachments, _sessionId, handlers) {
      // 让 mock 行为接近真实 fetch 流：delta 会在调用栈释放后到达。
      await Promise.resolve();
      handlers?.onDelta?.(prompt);
      return prompt;
    },
  };
}
