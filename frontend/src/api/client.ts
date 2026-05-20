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

export type ModelConfig = {
  provider_name: string;
  provider_base_url: string;
  provider_api_key?: string;
  default_model: string;
  allowed_models: string[];
  allow_streaming: boolean;
  request_timeout_seconds: number;
};

export type CreateInviteInput = {
  expires_at: number;
  max_uses: number;
};

export type ApiClient = {
  me: () => Promise<User | null>;
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
  startHermesInstance: (userId: string) => Promise<HermesInstance>;
  stopHermesInstance: (userId: string) => Promise<HermesInstance>;
  rebuildHermesInstance: (userId: string) => Promise<HermesInstance>;
  listChannels: () => Promise<Channel[]>;
  createChannel: (name: string, description?: string) => Promise<Channel>;
  listSessions: (channelId: string) => Promise<ChannelSession[]>;
  createSession: (
    channelId: string,
    kind: "chat" | "agent",
    title?: string,
  ) => Promise<ChannelSession>;
  workspaceStatus: () => Promise<HermesInstance | null>;
  ensureHermes: () => Promise<HermesInstance>;
  modelConfig: () => Promise<ModelConfig>;
  updateModelConfig: (config: ModelConfig) => Promise<void>;
  sendHermesPrompt: (prompt: string) => Promise<string>;
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

export function createApiClient(): ApiClient {
  return {
    async me() {
      const payload = await request<{ user: User } | null>("/api/auth/me", {
        allowUnauthorized: true,
      });
      return payload?.user ?? null;
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
      const payload = await request<{ model_config: ModelConfig }>(
        "/api/admin/model-config",
      );
      return { ...payload.model_config, provider_api_key: "" };
    },
    async updateModelConfig(config) {
      await request<void>("/api/admin/model-config", {
        method: "PUT",
        body: config,
      });
    },
    async sendHermesPrompt(prompt) {
      const response = await fetch("/api/hermes/v1/runs?stream=true", {
        method: "POST",
        credentials: "include",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ prompt, stream: true }),
      });

      if (!response.ok) {
        throw new Error(`Hermes request failed: ${response.status}`);
      }

      return response.text();
    },
  };
}

export function createMockApiClient(): ApiClient {
  let currentUser: User | null = {
    id: "user-1",
    email: "admin@example.com",
    role: "admin",
    status: "active",
  };
  let channels: Channel[] = [
    {
      id: "channel-1",
      name: "Research",
      description: "Default research channel",
    },
  ];
  let sessions: ChannelSession[] = [{ id: "session-1", channel_id: "channel-1", kind: "agent", title: "Session" }];
  let invites: Invite[] = [];
  let instance: HermesInstance | null = {
    id: "instance-1",
    user_id: "user-1",
    kind: "managed_docker",
    status: "running",
    base_url: "http://hermes-user-user-1:8000",
  };
  let modelConfig: ModelConfig = {
    provider_name: "openai-compatible",
    provider_base_url: "https://provider.example/v1",
    provider_api_key: "provider-secret",
    default_model: "gpt-4.1-mini",
    allowed_models: ["gpt-4.1-mini"],
    allow_streaming: true,
    request_timeout_seconds: 60,
  };

  return {
    async me() {
      return currentUser;
    },
    async login(email) {
      currentUser = { id: "user-1", email, role: "admin", status: "active" };
      return currentUser;
    },
    async bootstrapRegister(email) {
      currentUser = { id: "user-1", email, role: "admin", status: "active" };
      return currentUser;
    },
    async registerWithInvite(_inviteToken, email) {
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
      return session;
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
    async updateModelConfig(config) {
      modelConfig = config;
    },
    async sendHermesPrompt(prompt) {
      return `event: message\ndata: ${prompt}\n\n`;
    },
  };
}
