export type User = {
  id: string;
  email: string;
  role: "admin" | "user";
  status: "active" | "disabled";
};

export type Channel = {
  id: string;
  name: string;
  description?: string | null;
};

export type ChannelSession = {
  id: string;
  kind: "chat" | "agent";
  title?: string | null;
};

export type HermesInstance = {
  id: string;
  user_id: string;
  kind: "external" | "managed_docker";
  status: "provisioning" | "running" | "stopped" | "error";
  base_url: string;
};

export type ModelConfig = {
  provider_name: string;
  provider_base_url: string;
  default_model: string;
  allowed_models: string[];
};

export type ApiClient = {
  me: () => Promise<User | null>;
  listUsers: () => Promise<User[]>;
  listChannels: () => Promise<Channel[]>;
  listSessions: (channelId: string) => Promise<ChannelSession[]>;
  workspaceStatus: () => Promise<HermesInstance | null>;
  modelConfig: () => Promise<ModelConfig>;
};

async function request<T>(path: string): Promise<T> {
  const response = await fetch(path, { credentials: "include" });
  if (!response.ok) {
    throw new Error(`Request failed: ${response.status}`);
  }
  return response.json() as Promise<T>;
}

export function createApiClient(): ApiClient {
  return {
    async me() {
      const payload = await request<{ user: User }>("/api/auth/me");
      return payload.user;
    },
    async listUsers() {
      const payload = await request<{ users: User[] }>("/api/admin/users");
      return payload.users;
    },
    async listChannels() {
      const payload = await request<{ channels: Channel[] }>("/api/channels");
      return payload.channels;
    },
    async listSessions(channelId: string) {
      const payload = await request<{ sessions: ChannelSession[] }>(
        `/api/channels/${channelId}/sessions`,
      );
      return payload.sessions;
    },
    async workspaceStatus() {
      const payload = await request<{ hermes_instance: HermesInstance | null }>(
        "/api/workspace/status",
      );
      return payload.hermes_instance;
    },
    async modelConfig() {
      const payload = await request<{ model_config: ModelConfig }>(
        "/api/admin/model-config",
      );
      return payload.model_config;
    },
  };
}

export function createMockApiClient(): ApiClient {
  const channel: Channel = {
    id: "channel-1",
    name: "Research",
    description: "Default research channel",
  };

  return {
    async me() {
      return {
        id: "user-1",
        email: "admin@example.com",
        role: "admin",
        status: "active",
      };
    },
    async listUsers() {
      return [
        {
          id: "user-1",
          email: "admin@example.com",
          role: "admin",
          status: "active",
        },
      ];
    },
    async listChannels() {
      return [channel];
    },
    async listSessions() {
      return [{ id: "session-1", kind: "agent", title: "Session" }];
    },
    async workspaceStatus() {
      return {
        id: "instance-1",
        user_id: "user-1",
        kind: "managed_docker",
        status: "running",
        base_url: "http://hermes-user-user-1:8000",
      };
    },
    async modelConfig() {
      return {
        provider_name: "openai-compatible",
        provider_base_url: "https://provider.example/v1",
        default_model: "gpt-4.1-mini",
        allowed_models: ["gpt-4.1-mini"],
      };
    },
  };
}
