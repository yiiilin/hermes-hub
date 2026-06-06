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
  is_home?: boolean;
  deletable?: boolean;
  created_at?: number;
  updated_at?: number;
  recycle_at?: number | null;
};

export type SessionSummary = {
  id: string;
  title?: string | null;
  is_home?: boolean;
  deletable?: boolean;
  created_at?: number;
  updated_at?: number;
  recycle_at?: number | null;
};

export type ChannelMessage = {
  id: string;
  session_id: string;
  role: "user" | "assistant";
  message_kind?: "text" | "execution";
  client_message_key?: string | null;
  content: string;
  attachments: HermesAttachment[];
  created_at: number;
  updated_at?: number;
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
  kind: "managed_docker";
  status: "provisioning" | "running" | "stopped" | "error";
  name?: string;
  container_id?: string | null;
  health_status?: string;
  status_message?: string | null;
  runtime_image?: string | null;
  runtime_version?: string | null;
  last_user_activity_at?: number | null;
  last_started_at?: number | null;
  last_stopped_at?: number | null;
  stopped_reason?: string | null;
};

export type PublicPlatformHermesStatus = {
  enabled: boolean;
  ready: boolean;
  hermes_instance?: HermesInstance | null;
};

export type PublicPlatformSessionSummary = {
  id: string;
  title?: string | null;
  created_at: number;
  updated_at: number;
  recycle_at: number;
  public_url: string;
};

export type PublicPlatformSessionPage = {
  sessions: PublicPlatformSessionSummary[];
  page: number;
  page_size: number;
  total: number;
  total_pages: number;
};

export type PublicPlatformSessionListInput = {
  page?: number;
  pageSize?: number;
};

export type HermesScheduledTaskSnapshot = {
  id: string;
  name: string;
  enabled: boolean;
  schedule: string;
  timezone: string;
  next_run_at?: number | string | null;
  last_run_at?: number | string | null;
  status: string;
  source: string;
};

export type HermesSchedulerSnapshot = {
  user_id: string;
  user_email?: string | null;
  hermes_instance_id: string;
  instance_status: string;
  scheduler_enabled: boolean;
  running_jobs_count: number;
  reported_at: number | string;
  tasks: HermesScheduledTaskSnapshot[];
};

export type ModelConfigKind = "llm" | "image" | "title";
export type ModelApiType = "chat_completions" | "responses" | "images_generations";
export type ReasoningEffort = "minimal" | "low" | "medium" | "high";

export type ModelFallbackConfig = {
  enabled: boolean;
  provider_name: string;
  provider_base_url: string;
  provider_api_key?: string;
  default_model: string;
  allowed_models: string[];
  api_type: ModelApiType;
  reasoning_effort?: ReasoningEffort | null;
  allow_streaming: boolean;
  request_timeout_seconds: number;
  context_window_tokens: number;
  max_output_tokens: number;
  temperature: number;
  supports_parallel_tools: boolean;
};

export type ModelConfig = {
  config_kind: ModelConfigKind;
  enabled: boolean;
  provider_name: string;
  provider_base_url: string;
  provider_api_key?: string;
  default_model: string;
  allowed_models: string[];
  api_type: ModelApiType;
  reasoning_effort?: ReasoningEffort | null;
  allow_streaming: boolean;
  request_timeout_seconds: number;
  context_window_tokens: number;
  max_output_tokens: number;
  temperature: number;
  supports_parallel_tools: boolean;
  fallback?: ModelFallbackConfig | null;
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
  max_attachment_upload_bytes: number;
  attachment_retention_days: number;
  empty_chat_prompt: string;
  speech_input: SpeechInputSettings;
  public_platform: PublicPlatformSettings;
  api_management: ApiManagementSettings;
  business_oauth: BusinessOAuthSettings;
  oidc: OidcSettings;
  ldap: LdapSettings;
};

export type SystemParametersSettings = {
  max_sessions_per_user: number;
  max_attachment_upload_bytes: number;
  attachment_retention_days: number;
  empty_chat_prompt: string;
  speech_input: SpeechInputSettings;
};

export type AuthSettings = {
  oidc: OidcSettings;
  ldap: LdapSettings;
  business_oauth: BusinessOAuthSettings;
};

export type UpdatePublicPlatformSettingsRequest = {
  public_platform: PublicPlatformSettings;
};

export type UpdateApiManagementSettingsRequest = {
  api_management: ApiManagementSettings;
};

export type SpeechInputSettings = {
  enabled: boolean;
};

export type PublicPlatformSettings = {
  enabled: boolean;
  temporary_session_retention_hours: number;
};

export type ApiManagementSettings = {
  enabled: boolean;
};

export type BusinessOAuthSettings = {
  enabled: boolean;
  client_id: string;
  client_secret: string;
  allowed_redirect_uris: string[];
  scopes: string;
  authorization_code_ttl_seconds: number;
  hidden_session_idle_timeout_seconds: number;
  toolset_names: string[];
};

export type SpeechInputConfig = {
  enabled: boolean;
  runtime_available: boolean;
  max_duration_seconds: number;
  sample_rate: number;
  model: string;
};

export type SpeechInputStreamHandlers = {
  onClose: () => void;
  onDone: () => void;
  onError: (message: string) => void;
  onFinal: (text: string) => void;
  onOpen: () => void;
  onPartial: (text: string) => void;
};

export type SpeechInputStreamConnection = {
  close: () => void;
  sendAudio: (audio: ArrayBuffer) => void;
  sendStart: (sampleRate: number) => void;
  stop: () => void;
};

// Hermes Profile 前端只管理 SOUL.md；旧后端字段在读取时兼容忽略。
export type HermesProfile = {
  soul_md: string;
};

type HermesProfilePayload = HermesProfile & {
  agents_md?: string;
};

export type ManagedSkill = {
  path: string;
  size: number;
};

export type ManagedSkillTreeNode = {
  name: string;
  path: string;
  kind: "dir" | "file";
  size: number;
  children: ManagedSkillTreeNode[];
};

export type ManagedSkillContent = {
  path: string;
  content: string;
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
  allow_password_login: boolean;
};

export type LdapSettings = {
  enabled: boolean;
  display_name: string;
  url: string;
  bind_dn: string;
  bind_password: string;
  base_dn: string;
  user_filter: string;
  email_attribute: string;
  auto_create_users: boolean;
};

export type LdapPublicConfig = {
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
      session?: SessionSummary;
    }
  | { type: "message_created"; message: ChannelMessage }
  | { type: "message_updated"; message: ChannelMessage }
  | { type: "session_updated"; session: SessionSummary }
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

export type CreateInviteInput = {
  expires_at: number;
  max_uses: number;
};

export type PublicSessionRequestOptions = {
  includePublicToken?: boolean;
  sessionId?: string | null;
};

export type BootstrapStatus = {
  bootstrap_open: boolean;
  public_platform_enabled: boolean;
  empty_chat_prompt?: string | null;
};

export type ApiClient = {
  me: () => Promise<User | null>;
  bootstrapStatus: () => Promise<BootstrapStatus>;
  oidcConfig: () => Promise<OidcPublicConfig>;
  ldapConfig: () => Promise<LdapPublicConfig>;
  login: (email: string, password: string) => Promise<User>;
  ldapLogin: (email: string, password: string) => Promise<User>;
  bootstrapRegister: (email: string, password: string) => Promise<User>;
  registerWithInvite: (inviteToken: string, email: string, password: string) => Promise<User>;
  updatePassword: (newPassword: string) => Promise<void>;
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
  publicPlatformHermesInstance: () => Promise<PublicPlatformHermesStatus>;
  rebuildPublicPlatformHermesInstance: () => Promise<HermesInstance>;
  listPublicPlatformSessions: (
    input?: PublicPlatformSessionListInput,
  ) => Promise<PublicPlatformSessionPage>;
  forceClearPublicPlatformSession: (sessionId: string) => Promise<void>;
  listHermesSchedulerSnapshots: () => Promise<HermesSchedulerSnapshot[]>;
  workspaceHermesSchedulerSnapshot: () => Promise<HermesSchedulerSnapshot | null>;
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
  testModelFallbackConfig: (config: ModelConfig) => Promise<ModelConfigTestResult>;
  systemSettings: () => Promise<SystemSettings>;
  updateSystemParameters: (settings: SystemParametersSettings) => Promise<void>;
  updateAuthSettings: (settings: AuthSettings) => Promise<void>;
  updatePublicPlatformSettings: (settings: UpdatePublicPlatformSettingsRequest) => Promise<void>;
  updateApiManagementSettings: (settings: UpdateApiManagementSettingsRequest) => Promise<void>;
  updateSystemSettings: (settings: SystemSettings) => Promise<void>;
  hermesProfile: () => Promise<HermesProfile>;
  updateHermesProfile: (profile: HermesProfile) => Promise<void>;
  listManagedSkills: () => Promise<ManagedSkill[]>;
  listManagedSkillTree: () => Promise<ManagedSkillTreeNode>;
  readManagedSkill: (path: string) => Promise<ManagedSkillContent>;
  saveManagedSkill: (path: string, content: string) => Promise<ManagedSkillContent>;
  deleteManagedSkill: (path: string) => Promise<void>;
  createManagedSkillDirectory: (path: string) => Promise<void>;
  uploadManagedSkills: (files: File[], targetPath?: string) => Promise<ManagedSkill[]>;
  activeHermesRun: (channelId: string, sessionId: string) => Promise<HermesActiveRun | null>;
  subscribeSessionEvents: (
    channelId: string,
    sessionId: string,
    onEvent: (event: ChannelSessionEvent) => void,
    onError?: (error: Error) => void,
  ) => () => void;
  stopHermesRun: (channelId: string, sessionId: string) => Promise<HermesActiveRun | null>;
  clearHermesRun: (channelId: string, sessionId: string) => Promise<void>;
  listSessionsPublic: (options?: PublicSessionRequestOptions) => Promise<SessionSummary[]>;
  createSessionPublic: (
    kind?: "chat" | "agent",
    title?: string,
    options?: PublicSessionRequestOptions,
  ) => Promise<SessionSummary>;
  deleteSessionPublic: (sessionId: string, options?: PublicSessionRequestOptions) => Promise<void>;
  appendSessionMessagePublic: (
    sessionId: string,
    input: {
      role: "user" | "assistant";
      content: string;
      attachments?: HermesAttachment[];
      clientMessageKey?: string;
    },
    options?: PublicSessionRequestOptions,
  ) => Promise<ChannelMessage>;
  updateSessionMessagePublic: (
    sessionId: string,
    messageId: string,
    input: {
      content: string;
      attachments?: HermesAttachment[];
    },
    options?: PublicSessionRequestOptions,
  ) => Promise<ChannelMessage>;
  uploadSessionAttachmentsPublic: (
    sessionId: string,
    files: File[],
    options?: PublicSessionRequestOptions,
  ) => Promise<HermesAttachment[]>;
  speechInputConfig: () => Promise<SpeechInputConfig>;
  openSpeechInputStream: (handlers: SpeechInputStreamHandlers) => SpeechInputStreamConnection;
  subscribeSessionEventsPublic: (
    sessionId: string,
    onEvent: (event: ChannelSessionEvent) => void,
    onError?: (error: Error) => void,
    options?: PublicSessionRequestOptions,
  ) => () => void;
  stopSessionRunPublic: (sessionId: string, options?: PublicSessionRequestOptions) => Promise<void>;
};

type RequestOptions = {
  method?: "GET" | "POST" | "PUT" | "DELETE";
  body?: unknown;
  allowUnauthorized?: boolean;
  publicSessionToken?: boolean;
};

type ApiErrorPayload = {
  error?: string;
  message?: string;
  max_sessions_per_user?: number;
};

const PUBLIC_SESSION_TOKEN_STORAGE_KEY = "hermes-hub-public-session-token";
const PUBLIC_SESSION_TOKEN_HEADER = "X-Hermes-Hub-Public-Session";

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
  const headers = requestHeaders(options);
  const response = await fetch(path, {
    method: options.method ?? "GET",
    credentials: "include",
    headers,
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  });

  if (response.status === 401 && options.allowUnauthorized) {
    return null as T;
  }

  if (!response.ok) {
    if (response.status === 401 && options.publicSessionToken) {
      clearPublicSessionToken();
    }
    const payload: ApiErrorPayload = await response
      .json()
      .then(
        (value): ApiErrorPayload =>
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

async function requestForm<T>(path: string, form: FormData): Promise<T> {
  const response = await fetch(path, {
    method: "POST",
    credentials: "include",
    body: form,
  });

  if (!response.ok) {
    const payload: ApiErrorPayload = await response
      .json()
      .then(
        (value): ApiErrorPayload =>
          value && typeof value === "object"
            ? (value as ApiErrorPayload)
            : { message: response.statusText },
      )
      .catch((): ApiErrorPayload => ({ message: response.statusText }));
    const message = payload.message ?? payload.error ?? response.statusText;
    throw new ApiRequestError(String(message), payload);
  }

  return response.json() as Promise<T>;
}

function requestHeaders(options: RequestOptions): Record<string, string> | undefined {
  const headers: Record<string, string> = {};
  if (options.body !== undefined) {
    headers["Content-Type"] = "application/json";
  }
  if (options.publicSessionToken) {
    const token = readPublicSessionToken();
    if (token) {
      headers[PUBLIC_SESSION_TOKEN_HEADER] = token;
    }
  }
  return Object.keys(headers).length > 0 ? headers : undefined;
}

function defaultHubChannel(): Channel {
  // channel 已经是后端内部路由细节；旧签名只保留一个稳定占位，避免调用方继续关心真实 channel。
  return {
    id: "hermes-hub",
    name: "hermes-hub",
    description: "Hermes Hub default channel",
  };
}

function channelSessionFromSummary(
  session: SessionSummary,
  channelId = defaultHubChannel().id,
): ChannelSession {
  return {
    ...session,
    channel_id: channelId,
    kind: "agent",
  };
}

function readPublicSessionToken(): string | null {
  try {
    const token = globalThis.localStorage?.getItem(PUBLIC_SESSION_TOKEN_STORAGE_KEY)?.trim();
    return token || null;
  } catch {
    return null;
  }
}

function writePublicSessionToken(token: string | null | undefined) {
  const normalized = token?.trim();
  try {
    if (normalized) {
      globalThis.localStorage?.setItem(PUBLIC_SESSION_TOKEN_STORAGE_KEY, normalized);
    } else {
      globalThis.localStorage?.removeItem(PUBLIC_SESSION_TOKEN_STORAGE_KEY);
    }
  } catch {
    // localStorage 可能被浏览器策略禁用，保留 cookie 兼容路径即可。
  }
}

function clearPublicSessionToken() {
  try {
    globalThis.localStorage?.removeItem(PUBLIC_SESSION_TOKEN_STORAGE_KEY);
  } catch {
    // 忽略存储异常，401 本身会继续按 API 错误抛出。
  }
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
    enabled: config.config_kind === "image" ? config.enabled : true,
    api_type: config.config_kind === "image" ? "images_generations" : config.api_type,
    reasoning_effort: config.config_kind === "image" ? null : config.reasoning_effort,
    context_window_tokens: positiveNumberOrDefault(config.context_window_tokens, 128000),
    max_output_tokens: positiveNumberOrDefault(config.max_output_tokens, 4096),
    temperature: boundedNumberOrDefault(config.temperature, 0.7, 0, 2),
    supports_parallel_tools:
      config.config_kind === "llm" ? config.supports_parallel_tools !== false : false,
    allowed_models: [config.default_model],
    fallback: normalizedModelFallbackConfig(config),
  };
}

function modelConfigFromPayload(config: ModelConfig): ModelConfig {
  return {
    ...config,
    enabled: config.config_kind === "image" ? Boolean(config.enabled) : true,
    context_window_tokens: positiveNumberOrDefault(config.context_window_tokens, 128000),
    max_output_tokens: positiveNumberOrDefault(config.max_output_tokens, 4096),
    temperature: boundedNumberOrDefault(config.temperature, 0.7, 0, 2),
    supports_parallel_tools:
      config.config_kind === "llm" ? config.supports_parallel_tools !== false : false,
    fallback: modelFallbackConfigFromPayload(config),
  };
}

function normalizedModelFallbackConfig(config: ModelConfig): ModelFallbackConfig | null {
  if (config.config_kind === "image" || !config.fallback) {
    return null;
  }
  const fallback = config.fallback;
  return {
    ...fallback,
    enabled: Boolean(fallback.enabled),
    provider_api_key: fallback.provider_api_key ?? "",
    api_type: fallback.api_type || config.api_type,
    reasoning_effort: fallback.reasoning_effort ?? null,
    allowed_models: fallback.default_model ? [fallback.default_model] : [],
    request_timeout_seconds: positiveNumberOrDefault(fallback.request_timeout_seconds, 60),
    context_window_tokens: positiveNumberOrDefault(fallback.context_window_tokens, 128000),
    max_output_tokens: positiveNumberOrDefault(fallback.max_output_tokens, 4096),
    temperature: boundedNumberOrDefault(fallback.temperature, 0.7, 0, 2),
    supports_parallel_tools:
      config.config_kind === "llm" ? fallback.supports_parallel_tools !== false : false,
  };
}

function modelFallbackConfigFromPayload(config: ModelConfig): ModelFallbackConfig | null {
  if (config.config_kind === "image" || !config.fallback) {
    return null;
  }
  const fallback = config.fallback;
  return {
    ...fallback,
    enabled: Boolean(fallback.enabled),
    provider_api_key: fallback.provider_api_key ?? "",
    request_timeout_seconds: positiveNumberOrDefault(fallback.request_timeout_seconds, 60),
    context_window_tokens: positiveNumberOrDefault(fallback.context_window_tokens, 128000),
    max_output_tokens: positiveNumberOrDefault(fallback.max_output_tokens, 4096),
    temperature: boundedNumberOrDefault(fallback.temperature, 0.7, 0, 2),
    supports_parallel_tools:
      config.config_kind === "llm" ? fallback.supports_parallel_tools !== false : false,
  };
}

function positiveNumberOrDefault(value: number | undefined, fallback: number) {
  return Number.isFinite(value) && Number(value) > 0 ? Number(value) : fallback;
}

function boundedNumberOrDefault(
  value: number | undefined,
  fallback: number,
  min: number,
  max: number,
) {
  if (!Number.isFinite(value)) {
    return fallback;
  }
  return Math.min(max, Math.max(min, Number(value)));
}

function managedSkillUrl(path: string): string {
  return `/api/admin/managed-skills/${path
    .split("/")
    .filter(Boolean)
    .map(encodeURIComponent)
    // 点号段可能被浏览器在发出请求前规范化，显式编码后交给后端统一校验。
    .map((segment) => segment.replace(/\./g, "%2E"))
    .join("/")}`;
}

function managedSkillDirectoryUrl(path: string): string {
  return `/api/admin/managed-skills/directories/${path
    .split("/")
    .filter(Boolean)
    .map(encodeURIComponent)
    .map((segment) => segment.replace(/\./g, "%2E"))
    .join("/")}`;
}

function managedSkillFileName(file: File): string {
  return (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name;
}

function isManagedSkillZipFile(file: File): boolean {
  const fileName = managedSkillFileName(file).split("/").pop()?.toLowerCase() ?? "";
  return (
    fileName.endsWith(".zip") ||
    file.type === "application/zip" ||
    file.type === "application/x-zip-compressed"
  );
}

function hasHiddenManagedSkillSegment(path: string): boolean {
  return path
    .split("/")
    .filter(Boolean)
    .some((segment) => segment.startsWith("."));
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

export function defaultLdapSettings(): LdapSettings {
  return {
    enabled: false,
    display_name: "LDAP",
    url: "",
    bind_dn: "",
    bind_password: "",
    base_dn: "",
    user_filter: "(mail={email})",
    email_attribute: "mail",
    auto_create_users: true,
  };
}

export function defaultSpeechInputSettings(): SpeechInputSettings {
  return {
    enabled: false,
  };
}

export function defaultPublicPlatformSettings(): PublicPlatformSettings {
  return {
    enabled: false,
    temporary_session_retention_hours: 24,
  };
}

export function defaultApiManagementSettings(): ApiManagementSettings {
  return {
    enabled: false,
  };
}

export function defaultBusinessOAuthSettings(): BusinessOAuthSettings {
  return {
    enabled: false,
    client_id: "",
    client_secret: "",
    allowed_redirect_uris: [],
    scopes: "openid profile email",
    authorization_code_ttl_seconds: 600,
    hidden_session_idle_timeout_seconds: 3600,
    toolset_names: [],
  };
}

type SystemSettingsPayload = Partial<
  Omit<
    SystemSettings,
    "oidc" | "ldap" | "speech_input" | "public_platform" | "api_management" | "business_oauth"
  >
> & {
  oidc?: Partial<OidcSettings> | null;
  ldap?: Partial<LdapSettings> | null;
  speech_input?: Partial<SpeechInputSettings> | null;
  public_platform?: Partial<PublicPlatformSettings> | null;
  api_management?: Partial<ApiManagementSettings> | null;
  business_oauth?: Partial<BusinessOAuthSettings> | null;
};

function systemSettingsFromPayload(settings: SystemSettingsPayload): SystemSettings {
  return {
    max_sessions_per_user: positiveNumberOrDefault(settings.max_sessions_per_user, 20),
    max_attachment_upload_bytes: positiveNumberOrDefault(
      settings.max_attachment_upload_bytes,
      200 * 1024 * 1024,
    ),
    attachment_retention_days: positiveNumberOrDefault(settings.attachment_retention_days, 7),
    empty_chat_prompt:
      typeof settings.empty_chat_prompt === "string" ? settings.empty_chat_prompt : "",
    speech_input: {
      ...defaultSpeechInputSettings(),
      ...(settings.speech_input ?? {}),
    },
    public_platform: {
      ...defaultPublicPlatformSettings(),
      ...(settings.public_platform ?? {}),
    },
    api_management: {
      ...defaultApiManagementSettings(),
      ...(settings.api_management ?? {}),
    },
    business_oauth: {
      ...defaultBusinessOAuthSettings(),
      ...(settings.business_oauth ?? {}),
    },
    oidc: { ...defaultOidcSettings(), ...(settings.oidc ?? {}) },
    ldap: { ...defaultLdapSettings(), ...(settings.ldap ?? {}) },
  };
}

function speechInputConfigFromPayload(payload: {
  enabled?: boolean;
  runtime_available?: boolean;
  max_duration_seconds?: number;
  sample_rate?: number;
  model?: string;
}): SpeechInputConfig {
  return {
    enabled: Boolean(payload.enabled),
    runtime_available: Boolean(payload.runtime_available),
    max_duration_seconds: positiveNumberOrDefault(payload.max_duration_seconds, 60),
    sample_rate: positiveNumberOrDefault(payload.sample_rate, 16000),
    model: typeof payload.model === "string" ? payload.model : "",
  };
}

function speechInputStreamUrl(): string {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/api/speech-input/stream`;
}

function parseSpeechInputStreamMessage(raw: unknown): { type?: string; text?: string; message?: string } {
  if (typeof raw !== "string") {
    return {};
  }
  try {
    const payload = JSON.parse(raw) as { type?: unknown; text?: unknown; message?: unknown };
    return {
      message: typeof payload.message === "string" ? payload.message : undefined,
      text: typeof payload.text === "string" ? payload.text : undefined,
      type: typeof payload.type === "string" ? payload.type : undefined,
    };
  } catch {
    return {};
  }
}

function openSpeechInputStream(handlers: SpeechInputStreamHandlers): SpeechInputStreamConnection {
  const socket = new WebSocket(speechInputStreamUrl());
  socket.binaryType = "arraybuffer";
  socket.onopen = () => handlers.onOpen();
  socket.onerror = () => handlers.onError("speech input stream failed");
  socket.onclose = () => handlers.onClose();
  socket.onmessage = (event) => {
    const message = parseSpeechInputStreamMessage(event.data);
    if (message.type === "partial") {
      handlers.onPartial(message.text ?? "");
      return;
    }
    if (message.type === "final") {
      handlers.onFinal(message.text ?? "");
      return;
    }
    if (message.type === "done") {
      handlers.onDone();
      return;
    }
    if (message.type === "error") {
      handlers.onError(message.message ?? "speech input stream failed");
    }
  };
  const sendJson = (payload: unknown) => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify(payload));
    }
  };
  return {
    close() {
      socket.close();
    },
    sendAudio(audio) {
      if (socket.readyState === WebSocket.OPEN) {
        socket.send(audio);
      }
    },
    sendStart(sampleRate) {
      sendJson({ type: "start", sample_rate: sampleRate });
    },
    stop() {
      sendJson({ type: "stop" });
    },
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
      return request<BootstrapStatus>("/api/auth/bootstrap-status");
    },
    async oidcConfig() {
      const payload = await request<{ oidc: OidcPublicConfig }>("/api/auth/oidc/config");
      return payload.oidc;
    },
    async ldapConfig() {
      const payload = await request<{ ldap: LdapPublicConfig }>("/api/auth/ldap/config");
      return payload.ldap;
    },
    async login(email, password) {
      const payload = await request<{ user: User }>("/api/auth/login", {
        method: "POST",
        body: { email, password },
      });
      return payload.user;
    },
    async ldapLogin(email, password) {
      const payload = await request<{ user: User }>("/api/auth/ldap/login", {
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
    async updatePassword(newPassword) {
      await request<void>("/api/auth/password", {
        method: "PUT",
        body: { new_password: newPassword },
      });
    },
    async logout() {
      await request<void>("/api/auth/logout", { method: "POST" });
    },
    async listUsers() {
      const payload = await request<{ users: User[] }>("/api/admin/users");
      return payload.users;
    },
    async disableUser(userId) {
      const payload = await request<{ user: User }>(`/api/admin/users/${userId}/disable`, {
        method: "POST",
      });
      return payload.user;
    },
    async enableUser(userId) {
      const payload = await request<{ user: User }>(`/api/admin/users/${userId}/enable`, {
        method: "POST",
      });
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
      const payload = await request<{ invite: Invite }>(`/api/invites/${inviteId}/revoke`, {
        method: "POST",
      });
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
    async publicPlatformHermesInstance() {
      return request<PublicPlatformHermesStatus>("/api/admin/public-platform/hermes-instance");
    },
    async rebuildPublicPlatformHermesInstance() {
      const payload = await request<{ hermes_instance: HermesInstance }>(
        "/api/admin/public-platform/hermes-instance/rebuild",
        { method: "POST" },
      );
      return payload.hermes_instance;
    },
    async listPublicPlatformSessions(input) {
      return request<PublicPlatformSessionPage>(publicPlatformSessionsAdminPath(input));
    },
    async forceClearPublicPlatformSession(sessionId) {
      await request<void>(
        `/api/admin/public-platform/sessions/${encodeURIComponent(sessionId)}`,
        { method: "DELETE" },
      );
    },
    async listHermesSchedulerSnapshots() {
      const payload = await request<
        | HermesSchedulerSnapshot[]
        | {
            hermes_scheduler_snapshots?: HermesSchedulerSnapshot[];
            scheduler_snapshots?: HermesSchedulerSnapshot[];
            snapshots?: HermesSchedulerSnapshot[];
          }
      >("/api/admin/hermes-scheduler-snapshots");
      if (Array.isArray(payload)) {
        return payload;
      }
      // 兼容后端字段名小幅调整，避免只读页因为包裹 key 不一致而整体空白。
      return (
        payload.hermes_scheduler_snapshots ?? payload.scheduler_snapshots ?? payload.snapshots ?? []
      );
    },
    async workspaceHermesSchedulerSnapshot() {
      const payload = await request<{
        hermes_scheduler_snapshot?: HermesSchedulerSnapshot | null;
        scheduler_snapshot?: HermesSchedulerSnapshot | null;
      }>("/api/workspace/hermes-scheduler-snapshot");
      // 个人页只需要当前用户自己的快照；字段名保持和管理员接口的主字段一致。
      return payload.hermes_scheduler_snapshot ?? payload.scheduler_snapshot ?? null;
    },
    async listChannels() {
      // channel 已变成 Hub 内部路由细节；保留旧方法只是为了兼容仍按旧签名注入的测试/组件。
      return [defaultHubChannel()];
    },
    async createChannel(name, description) {
      // 前端不再创建真实 channel，返回调用方传入的展示信息即可。
      return { id: "hermes-hub", name: name.trim() || "hermes-hub", description };
    },
    async listSessions(channelId) {
      const payload = await request<{ sessions: SessionSummary[] }>("/api/sessions");
      return payload.sessions.map((session) => channelSessionFromSummary(session, channelId));
    },
    async createSession(channelId, kind, title) {
      const payload = await request<{ session: SessionSummary }>("/api/sessions", {
        method: "POST",
        body: { kind, title },
      });
      return channelSessionFromSummary(payload.session, channelId);
    },
    async deleteSession(_channelId, sessionId) {
      await request<void>(`/api/sessions/${sessionId}`, {
        method: "DELETE",
      });
    },
    async listSessionMessages(_channelId, sessionId) {
      const payload = await request<{ messages: ChannelMessage[] }>(
        `/api/sessions/${sessionId}/messages`,
      );
      return payload.messages;
    },
    async appendSessionMessage(_channelId, sessionId, input) {
      const payload = await request<{ message: ChannelMessage }>(
        `/api/sessions/${sessionId}/messages`,
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
    async updateSessionMessage(_channelId, sessionId, messageId, input) {
      const payload = await request<{ message: ChannelMessage }>(
        `/api/sessions/${sessionId}/messages/${messageId}`,
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
    async uploadSessionAttachments(_channelId, sessionId, files) {
      const form = new FormData();
      for (const file of files) {
        form.append("file", file, file.name);
      }

      const response = await fetch(`/api/sessions/${sessionId}/attachments`, {
        method: "POST",
        credentials: "include",
        body: form,
      });

      if (!response.ok) {
        const message = await response
          .json()
          .then((value) => value.message ?? value.error ?? response.statusText)
          .catch(() => response.statusText);
        throw new Error(String(message));
      }

      const payload = (await response.json()) as {
        attachments: HermesAttachment[];
      };
      return payload.attachments;
    },
    async createChannelRun(_channelId, sessionId, input) {
      return request<{ message: ChannelMessage; run: ChannelRun }>(
        `/api/sessions/${sessionId}/runs`,
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
      const payload = await request<{ session: SessionSummary }>(
        `/api/sessions/${sessionId}/title`,
        { method: "POST", body: { prompt } },
      );
      return channelSessionFromSummary(payload.session, channelId);
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
      const payload = await request<ModelConfigStatus>("/api/admin/model-config");
      const modelConfigs = (payload.model_configs ?? [payload.model_config]).map(
        modelConfigFromPayload,
      );
      return {
        ...payload,
        model_config: modelConfigFromPayload(payload.model_config),
        model_configs: modelConfigs,
        required_models_ready: payload.required_models_ready ?? false,
        missing_required_model_config_kinds: payload.missing_required_model_config_kinds ?? [],
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
      return request<ModelConfigTestResult>(`/api/admin/model-config/${config.config_kind}/test`, {
        method: "POST",
        body: normalizedModelConfig(config),
      });
    },
    async testModelFallbackConfig(config) {
      return request<ModelConfigTestResult>(
        `/api/admin/model-config/${config.config_kind}/fallback/test`,
        {
          method: "POST",
          body: normalizedModelConfig(config),
        },
      );
    },
    async systemSettings() {
      const payload = await request<{ settings: SystemSettingsPayload }>(
        "/api/admin/system-settings",
      );
      return systemSettingsFromPayload(payload.settings);
    },
    async updateSystemParameters(settings) {
      await request<void>("/api/admin/system-settings/system", {
        method: "PUT",
        body: settings,
      });
    },
    async updateAuthSettings(settings) {
      await request<void>("/api/admin/system-settings/auth", {
        method: "PUT",
        body: settings,
      });
    },
    async updatePublicPlatformSettings(settings) {
      await request<void>("/api/admin/system-settings/public-platform", {
        method: "PUT",
        body: settings,
      });
    },
    async updateApiManagementSettings(settings) {
      await request<void>("/api/admin/system-settings/api-management", {
        method: "PUT",
        body: settings,
      });
    },
    async updateSystemSettings(settings) {
      await request<void>("/api/admin/system-settings", {
        method: "PUT",
        body: settings,
      });
    },
    async hermesProfile() {
      const payload = await request<{ profile: HermesProfile }>("/api/admin/hermes-profile");
      return {
        soul_md: payload.profile.soul_md ?? "",
      };
    },
    async updateHermesProfile(profile) {
      await request<void>("/api/admin/hermes-profile", {
        method: "PUT",
        body: profile,
      });
    },
    async listManagedSkills() {
      const payload = await request<{ skills: ManagedSkill[] }>("/api/admin/managed-skills");
      return payload.skills;
    },
    async listManagedSkillTree() {
      const payload = await request<{ tree: ManagedSkillTreeNode }>(
        "/api/admin/managed-skills/tree",
      );
      return payload.tree;
    },
    async readManagedSkill(path) {
      const payload = await request<{ skill: ManagedSkillContent }>(managedSkillUrl(path));
      return payload.skill;
    },
    async saveManagedSkill(path, content) {
      const payload = await request<{ skill: ManagedSkillContent }>(managedSkillUrl(path), {
        method: "PUT",
        body: { content },
      });
      return payload.skill;
    },
    async deleteManagedSkill(path) {
      await request<void>(managedSkillUrl(path), { method: "DELETE" });
    },
    async createManagedSkillDirectory(path) {
      await request<void>(managedSkillDirectoryUrl(path), { method: "POST" });
    },
    async uploadManagedSkills(files, targetPath) {
      if (files.some(isManagedSkillZipFile)) {
        throw new Error("managed skill zip uploads are not supported");
      }
      const form = new FormData();
      if (targetPath?.trim()) {
        form.append("target_path", targetPath.trim());
      }
      for (const file of files) {
        form.append("files", file, managedSkillFileName(file));
      }
      const payload = await requestForm<{ skills: ManagedSkill[] }>(
        "/api/admin/managed-skills/upload",
        form,
      );
      return payload.skills;
    },
    async activeHermesRun(_channelId, sessionId) {
      const payload = await request<{ active_run: HermesActiveRun | null }>(
        `/api/sessions/${sessionId}/active-run`,
      );
      return payload.active_run;
    },
    subscribeSessionEvents(_channelId, sessionId, onEvent, onError) {
      const source = new EventSource(`/api/sessions/${sessionId}/events`, {
        withCredentials: true,
      });
      const eventNames = [
        "messages_snapshot",
        "message_created",
        "message_updated",
        "session_updated",
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
    async stopHermesRun(_channelId, sessionId) {
      await request<void>(`/api/sessions/${sessionId}/stop`, { method: "POST" });
      return null;
    },
    async clearHermesRun(_channelId, sessionId) {
      await request<void>(`/api/sessions/${sessionId}/active-run`, {
        method: "DELETE",
      });
    },
    async listSessionsPublic(options) {
      const payload = await request<{ sessions: SessionSummary[]; public_token?: string }>(
        publicSessionsPath(options),
        {
          publicSessionToken: options?.includePublicToken === true,
        },
      );
      if (options?.includePublicToken === true || payload.public_token !== undefined) {
        writePublicSessionToken(payload.public_token ?? null);
      }
      return payload.sessions;
    },
    async createSessionPublic(kind = "agent", title, options) {
      const payload = await request<{ session: SessionSummary; public_token?: string }>(
        "/api/sessions",
        {
          method: "POST",
          body: { kind, title },
          publicSessionToken: options?.includePublicToken === true,
        },
      );
      if (options?.includePublicToken === true || payload.public_token !== undefined) {
        writePublicSessionToken(payload.public_token ?? null);
      }
      return payload.session;
    },
    async deleteSessionPublic(sessionId, options) {
      await request<void>(`/api/sessions/${sessionId}`, {
        method: "DELETE",
        publicSessionToken: options?.includePublicToken === true,
      });
    },
    async appendSessionMessagePublic(sessionId, input, options) {
      const payload = await request<{ message: ChannelMessage }>(
        `/api/sessions/${sessionId}/messages`,
        {
          method: "POST",
          publicSessionToken: options?.includePublicToken === true,
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
    async updateSessionMessagePublic(sessionId, messageId, input, options) {
      const payload = await request<{ message: ChannelMessage }>(
        `/api/sessions/${sessionId}/messages/${messageId}`,
        {
          method: "PUT",
          publicSessionToken: options?.includePublicToken === true,
          body: {
            content: input.content,
            attachments: stripAttachmentPreviews(input.attachments ?? []),
          },
        },
      );
      return payload.message;
    },
    async uploadSessionAttachmentsPublic(sessionId, files, options) {
      const form = new FormData();
      for (const file of files) {
        form.append("file", file, file.name);
      }
      const publicToken = options?.includePublicToken === true ? readPublicSessionToken() : null;

      const response = await fetch(`/api/sessions/${sessionId}/attachments`, {
        method: "POST",
        credentials: "include",
        headers: publicToken ? { [PUBLIC_SESSION_TOKEN_HEADER]: publicToken } : undefined,
        body: form,
      });

      if (!response.ok) {
        if (response.status === 401 && options?.includePublicToken === true) {
          clearPublicSessionToken();
        }
        const message = await response
          .json()
          .then((value) => value.message ?? value.error ?? response.statusText)
          .catch(() => response.statusText);
        throw new Error(String(message));
      }

      const payload = (await response.json()) as {
        attachments: HermesAttachment[];
      };
      return payload.attachments;
    },
    async speechInputConfig() {
      const payload = await request<{ speech_input: SpeechInputConfig }>(
        "/api/speech-input/config",
      );
      return speechInputConfigFromPayload(payload.speech_input);
    },
    openSpeechInputStream(handlers) {
      return openSpeechInputStream(handlers);
    },
    subscribeSessionEventsPublic(sessionId, onEvent, onError) {
      const source = new EventSource(`/api/sessions/${sessionId}/events`, {
        withCredentials: true,
      });
      const eventNames = [
        "messages_snapshot",
        "message_created",
        "message_updated",
        "session_updated",
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
        onError?.(new Error("session event stream disconnected"));
      };
      return () => {
        for (const [eventName, listener] of listeners) {
          source.removeEventListener(eventName, listener);
        }
        source.close();
      };
    },
    async stopSessionRunPublic(sessionId, options) {
      await request<void>(`/api/sessions/${sessionId}/stop`, {
        method: "POST",
        publicSessionToken: options?.includePublicToken === true,
      });
    },
  };
}

function publicSessionsPath(options: PublicSessionRequestOptions | undefined) {
  const sessionId = options?.sessionId?.trim();
  return sessionId ? `/api/sessions?session_id=${encodeURIComponent(sessionId)}` : "/api/sessions";
}

function publicPlatformSessionsAdminPath(input: PublicPlatformSessionListInput | undefined) {
  const params = new URLSearchParams();
  if (input?.page !== undefined) {
    params.set("page", String(input.page));
  }
  if (input?.pageSize !== undefined) {
    params.set("page_size", String(input.pageSize));
  }
  const query = params.toString();
  return query
    ? `/api/admin/public-platform/sessions?${query}`
    : "/api/admin/public-platform/sessions";
}

function stripAttachmentPreviews(attachments: HermesAttachment[]): HermesAttachment[] {
  return attachments.map(({ data_url: _dataUrl, ...attachment }) => attachment);
}

type MockApiClientOptions = {
  initialUser?: User | null;
  oidcPublicConfig?: OidcPublicConfig;
  ldapPublicConfig?: LdapPublicConfig;
  ldapLogin?: ApiClient["ldapLogin"];
  bootstrapOpen?: boolean;
  requiredModelsReady?: boolean;
  missingRequiredModelConfigKinds?: ModelConfigKind[];
  initialInstance?: HermesInstance | null;
  publicPlatformInstance?: HermesInstance | null;
  initialMessagesBySessionId?: Record<string, ChannelMessage[]>;
  createChannelRun?: ApiClient["createChannelRun"];
  activeRunsBySessionId?: Record<string, HermesActiveRun>;
  subscribeSessionEvents?: ApiClient["subscribeSessionEvents"];
  stopHermesRun?: ApiClient["stopHermesRun"];
  deleteSession?: ApiClient["deleteSession"];
  createSession?: ApiClient["createSession"];
  initialManagedSkills?: Record<string, string>;
  initialManagedSkillDirectories?: string[];
  readManagedSkill?: ApiClient["readManagedSkill"];
  saveManagedSkill?: ApiClient["saveManagedSkill"];
  deleteManagedSkill?: ApiClient["deleteManagedSkill"];
  createManagedSkillDirectory?: ApiClient["createManagedSkillDirectory"];
  uploadManagedSkills?: ApiClient["uploadManagedSkills"];
  initialHermesProfile?: HermesProfilePayload;
  initialHermesSchedulerSnapshots?: HermesSchedulerSnapshot[];
  initialPublicPlatformSessions?: PublicPlatformSessionSummary[];
  publicPlatformSettings?: Partial<PublicPlatformSettings>;
  apiManagementSettings?: Partial<ApiManagementSettings>;
};

function withMockMessageKind(message: ChannelMessage): ChannelMessage {
  if (message.message_kind) {
    return message;
  }
  return {
    ...message,
    message_kind: inferMockMessageKind(message.role, message.client_message_key, message.content),
  };
}

function inferMockMessageKind(
  role: ChannelMessage["role"],
  clientMessageKey: string | null | undefined,
  content: string,
): ChannelMessage["message_kind"] {
  if (
    role === "assistant" &&
    (clientMessageKey?.startsWith("hermes-execution:") || isMockExecutionProtocolMessage(content))
  ) {
    return "execution";
  }
  return "text";
}

function isMockExecutionProtocolMessage(content: string) {
  const trimmed = content.trimStart();
  return (
    trimmed.startsWith("<!-- hermes-hub:execution:v1 -->") ||
    trimmed.startsWith("执行步骤\n") ||
    trimmed.split("\n").some(isMockLegacyExecutionLine)
  );
}

function isMockLegacyExecutionLine(line: string) {
  const trimmed = line.trim();
  const firstWhitespace = trimmed.search(/\s/);
  if (firstWhitespace < 0) {
    return false;
  }
  const rest = trimmed.slice(firstWhitespace).trimStart();
  const openParen = rest.indexOf("(");
  if (openParen <= 0 || !rest.endsWith(")")) {
    return false;
  }

  // 测试 mock 和后端保持同一类执行行判定，避免前端测试误用旧内容猜测。
  return /^[A-Za-z0-9_.-]+$/.test(rest.slice(0, openParen));
}

export function createMockApiClient(options: MockApiClientOptions = {}): ApiClient {
  let hasAnyUser = options.bootstrapOpen === true ? false : true;
  let currentUser: User | null =
    "initialUser" in options
      ? options.initialUser!
      : {
          id: "user-1",
          email: "admin@example.com",
          role: "admin",
          status: "active",
        };
  const usersByEmail = new Map<string, User>();
  if (currentUser) {
    usersByEmail.set(currentUser.email.toLowerCase(), currentUser);
  }
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
  const initialMessagesBySessionId: Record<string, ChannelMessage[]> = {
    "session-1": [],
    ...(options.initialMessagesBySessionId ?? {}),
  };
  let messagesBySessionId: Record<string, ChannelMessage[]> = {};
  for (const [sessionId, sessionMessages] of Object.entries(initialMessagesBySessionId)) {
    messagesBySessionId[sessionId] = sessionMessages.map(withMockMessageKind);
  }
  let activeRunsBySessionId = { ...(options.activeRunsBySessionId ?? {}) };
  const sessionEventListenersBySessionId: Record<
    string,
    Set<(event: ChannelSessionEvent) => void>
  > = {};
  let invites: Invite[] = [];
  let instance: HermesInstance | null =
    "initialInstance" in options
      ? options.initialInstance!
      : {
          id: "instance-1",
          user_id: "user-1",
          kind: "managed_docker",
          status: "running",
          runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:latest",
          runtime_version: null,
          last_user_activity_at: null,
          last_started_at: null,
          last_stopped_at: null,
          stopped_reason: null,
        };
  let publicPlatformInstance: HermesInstance | null =
    "publicPlatformInstance" in options ? options.publicPlatformInstance! : null;
  let publicPlatformSessions: PublicPlatformSessionSummary[] = [
    ...(options.initialPublicPlatformSessions ?? []),
  ];
  let modelConfig: ModelConfig = {
    config_kind: "llm",
    provider_name: "openai-compatible",
    provider_base_url: "https://ready-provider.example/v1",
    provider_api_key: "ready-provider-key",
    default_model: "gpt-4.1-mini",
    allowed_models: ["gpt-4.1-mini"],
    api_type: "chat_completions",
    reasoning_effort: null,
    enabled: true,
    allow_streaming: true,
    request_timeout_seconds: 60,
    context_window_tokens: 128000,
    max_output_tokens: 4096,
    temperature: 0.7,
    supports_parallel_tools: true,
    fallback: null,
  };
  let modelConfigs: ModelConfig[] = [
    modelConfig,
    {
      ...modelConfig,
      config_kind: "image",
      enabled: false,
      default_model: "gpt-image-1",
      allowed_models: ["gpt-image-1"],
      api_type: "images_generations",
      reasoning_effort: null,
      allow_streaming: false,
    },
    {
      ...modelConfig,
      config_kind: "title",
      enabled: true,
      api_type: "chat_completions",
      allow_streaming: false,
    },
  ];
  let systemSettings: SystemSettings = {
    max_sessions_per_user: 20,
    max_attachment_upload_bytes: 200 * 1024 * 1024,
    attachment_retention_days: 7,
    empty_chat_prompt: "",
    speech_input: defaultSpeechInputSettings(),
    public_platform: {
      ...defaultPublicPlatformSettings(),
      ...(options.publicPlatformSettings ?? {}),
    },
    api_management: {
      ...defaultApiManagementSettings(),
      ...(options.apiManagementSettings ?? {}),
    },
    business_oauth: defaultBusinessOAuthSettings(),
    oidc: defaultOidcSettings(),
    ldap: defaultLdapSettings(),
  };
  let managedSkills: Record<string, string> = {
    "writing/SKILL.md": "# Writing\n\nUse concise prose.\n",
    ...(options.initialManagedSkills ?? {}),
  };
  let managedSkillDirectories = new Set(options.initialManagedSkillDirectories ?? []);
  let hermesProfile: HermesProfile = {
    soul_md: options.initialHermesProfile?.soul_md ?? "",
  };
  const hermesSchedulerSnapshots = options.initialHermesSchedulerSnapshots ?? [];

  function emitSessionEvent(sessionId: string, event: ChannelSessionEvent) {
    for (const listener of sessionEventListenersBySessionId[sessionId] ?? []) {
      listener(event);
    }
  }

  function authenticateUserByEmail(email: string, fallbackRole: User["role"]): User {
    const normalizedEmail = email.trim().toLowerCase();
    const existingUser = usersByEmail.get(normalizedEmail);
    const user: User = existingUser ?? {
      id: `user-${usersByEmail.size + 1}`,
      email,
      role: fallbackRole,
      status: "active",
    };
    usersByEmail.set(normalizedEmail, user);
    hasAnyUser = true;
    currentUser = user;
    return user;
  }

  function isMockPublicPlatformReady() {
    return Boolean(
      systemSettings.public_platform.enabled && publicPlatformInstance?.status === "running",
    );
  }

  function publicPlatformSessionPage(input: PublicPlatformSessionListInput | undefined) {
    const page = Math.max(1, input?.page ?? 1);
    const pageSize = Math.max(1, input?.pageSize ?? 10);
    const sortedSessions = publicPlatformSessions
      .slice()
      .sort((left, right) => (right.updated_at ?? 0) - (left.updated_at ?? 0));
    const start = (page - 1) * pageSize;
    const total = sortedSessions.length;
    return {
      sessions: sortedSessions.slice(start, start + pageSize),
      page,
      page_size: pageSize,
      total,
      total_pages: total === 0 ? 0 : Math.ceil(total / pageSize),
    };
  }

  function managedSkillTreeFromState(): ManagedSkillTreeNode {
    const root: ManagedSkillTreeNode = {
      name: "",
      path: "",
      kind: "dir",
      size: 0,
      children: [],
    };

    function ensureDir(path: string) {
      let node = root;
      let currentPath = "";
      for (const segment of path.split("/").filter(Boolean)) {
        currentPath = currentPath ? `${currentPath}/${segment}` : segment;
        let child = node.children.find((item) => item.name === segment && item.kind === "dir");
        if (!child) {
          child = {
            name: segment,
            path: currentPath,
            kind: "dir",
            size: 0,
            children: [],
          };
          node.children.push(child);
        }
        node = child;
      }
      return node;
    }

    for (const directory of managedSkillDirectories) {
      if (!hasHiddenManagedSkillSegment(directory)) {
        ensureDir(directory);
      }
    }
    for (const [path, content] of Object.entries(managedSkills)) {
      if (hasHiddenManagedSkillSegment(path)) {
        continue;
      }
      const segments = path.split("/");
      const fileName = segments.pop()!;
      const parent = ensureDir(segments.join("/"));
      parent.children.push({
        name: fileName,
        path,
        kind: "file",
        size: new Blob([content]).size,
        children: [],
      });
    }

    function sortNode(node: ManagedSkillTreeNode) {
      node.children.sort((left, right) => {
        if (left.kind !== right.kind) {
          return left.kind === "dir" ? -1 : 1;
        }
        return left.name.localeCompare(right.name);
      });
      for (const child of node.children) {
        sortNode(child);
      }
    }
    sortNode(root);
    return root;
  }

  return {
    async me() {
      return currentUser;
    },
    async bootstrapStatus() {
      return {
        bootstrap_open: !hasAnyUser,
        public_platform_enabled: isMockPublicPlatformReady(),
        empty_chat_prompt: systemSettings.empty_chat_prompt,
      };
    },
    async oidcConfig() {
      return (
        options.oidcPublicConfig ?? {
          enabled: systemSettings.oidc.enabled,
          display_name: systemSettings.oidc.display_name,
          allow_password_login: systemSettings.oidc.allow_password_login,
        }
      );
    },
    async ldapConfig() {
      return (
        options.ldapPublicConfig ?? {
          enabled: systemSettings.ldap.enabled,
          display_name: systemSettings.ldap.display_name,
        }
      );
    },
    async login(email) {
      return authenticateUserByEmail(email, "admin");
    },
    async ldapLogin(email, password) {
      if (options.ldapLogin) {
        const user = await options.ldapLogin(email, password);
        usersByEmail.set(user.email.toLowerCase(), user);
        hasAnyUser = true;
        currentUser = user;
        return user;
      }
      return authenticateUserByEmail(email, "user");
    },
    async bootstrapRegister(email) {
      return authenticateUserByEmail(email, "admin");
    },
    async registerWithInvite(_inviteToken, email) {
      return authenticateUserByEmail(email, "user");
    },
    async updatePassword() {
      // mock 客户端不存明文密码；测试只需要确认调用链和错误处理。
    },
    async logout() {
      currentUser = null;
    },
    async listUsers() {
      return currentUser ? [currentUser] : [];
    },
    async disableUser(userId) {
      currentUser = {
        ...(currentUser as User),
        id: userId,
        status: "disabled",
      };
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
        runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:latest",
        runtime_version: null,
        last_user_activity_at: Date.now() / 1000,
        last_started_at: Date.now() / 1000,
        last_stopped_at: null,
        stopped_reason: null,
      };
      return instance;
    },
    async startHermesInstance() {
      const now = Date.now() / 1000;
      instance = {
        ...(instance as HermesInstance),
        status: "running",
        last_started_at: now,
        stopped_reason: null,
      };
      return instance;
    },
    async stopHermesInstance() {
      instance = {
        ...(instance as HermesInstance),
        status: "stopped",
        last_stopped_at: Date.now() / 1000,
        stopped_reason: "manual",
      };
      return instance;
    },
    async rebuildHermesInstance() {
      const now = Date.now() / 1000;
      instance = {
        ...(instance as HermesInstance),
        status: "running",
        last_started_at: now,
        stopped_reason: null,
      };
      return instance;
    },
    async publicPlatformHermesInstance() {
      return {
        enabled: systemSettings.public_platform.enabled,
        ready: isMockPublicPlatformReady(),
        hermes_instance: publicPlatformInstance,
      };
    },
    async rebuildPublicPlatformHermesInstance() {
      const now = Date.now() / 1000;
      publicPlatformInstance = {
        ...(publicPlatformInstance ?? {
          id: "public-instance-1",
          user_id: "public-user-1",
          kind: "managed_docker",
          runtime_image: "ghcr.io/yiiilin/hermes-hub-hermes:latest",
          runtime_version: null,
          last_user_activity_at: null,
          last_stopped_at: null,
          stopped_reason: null,
        }),
        status: "running",
        health_status: "starting",
        last_started_at: now,
      };
      return publicPlatformInstance;
    },
    async listPublicPlatformSessions(input) {
      return publicPlatformSessionPage(input);
    },
    async forceClearPublicPlatformSession(sessionId) {
      publicPlatformSessions = publicPlatformSessions.filter((session) => session.id !== sessionId);
      sessions = sessions.filter((session) => session.id !== sessionId);
      delete messagesBySessionId[sessionId];
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, {
        type: "session_deleted",
        session_id: sessionId,
      });
    },
    async listHermesSchedulerSnapshots() {
      return hermesSchedulerSnapshots;
    },
    async workspaceHermesSchedulerSnapshot() {
      const currentUserId = currentUser?.id;
      if (!currentUserId) {
        return null;
      }
      return (
        hermesSchedulerSnapshots.find((snapshot) => snapshot.user_id === currentUserId) ?? null
      );
    },
    async listChannels() {
      return channels;
    },
    async createChannel(name, description) {
      const channel = {
        id: `channel-${channels.length + 1}`,
        name,
        description,
      };
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
      emitSessionEvent(sessionId, {
        type: "session_deleted",
        session_id: sessionId,
      });
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

      const now = Date.now();
      const message: ChannelMessage = {
        id: `message-${(messagesBySessionId[sessionId] ?? []).length + 1}`,
        session_id: sessionId,
        role: input.role,
        message_kind: inferMockMessageKind(input.role, input.clientMessageKey, input.content),
        client_message_key: input.clientMessageKey,
        content: input.content,
        attachments: input.attachments ?? [],
        created_at: now,
        updated_at: now,
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
        message_kind: inferMockMessageKind(
          existing.role,
          existing.client_message_key,
          input.content,
        ),
        content: input.content,
        attachments: input.attachments ?? [],
        updated_at: Date.now(),
      };
      messagesBySessionId[sessionId] = messages.map((message) =>
        message.id === messageId ? nextMessage : message,
      );
      sessions = sessions.map((session) =>
        session.id === sessionId ? { ...session, updated_at: Date.now() } : session,
      );
      emitSessionEvent(sessionId, {
        type: "message_updated",
        message: nextMessage,
      });
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
        updated_at: Date.now(),
      };
      messagesBySessionId[sessionId] = [...(messagesBySessionId[sessionId] ?? []), assistant];
      emitSessionEvent(sessionId, {
        type: "message_created",
        message: assistant,
      });
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, {
        type: "run_cleared",
        session_id: sessionId,
      });
      return { message, run };
    },
    async generateSessionTitle(channelId, sessionId, prompt) {
      const session = sessions.find(
        (item) => item.id === sessionId && item.channel_id === channelId,
      );
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
        missing_required_model_config_kinds: options.missingRequiredModelConfigKinds ?? [],
      };
    },
    async modelConfigs() {
      return modelConfigs;
    },
    async updateModelConfig(config) {
      const normalized = normalizedModelConfig(config);
      modelConfigs = modelConfigs.map((existing) =>
        existing.config_kind === config.config_kind ? normalized : existing,
      );
      modelConfig = modelConfigs.find((existing) => existing.config_kind === "llm") ?? modelConfig;
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
    async testModelFallbackConfig() {
      return {
        ok: true,
        status_code: 200,
        message: "model test succeeded",
        duration_ms: 12,
      };
    },
    async systemSettings() {
      return systemSettingsFromPayload(systemSettings);
    },
    async updateSystemParameters(settings) {
      systemSettings = {
        ...systemSettings,
        max_sessions_per_user: settings.max_sessions_per_user,
        max_attachment_upload_bytes: settings.max_attachment_upload_bytes,
        attachment_retention_days: settings.attachment_retention_days,
        empty_chat_prompt: settings.empty_chat_prompt,
        speech_input: {
          ...systemSettings.speech_input,
          ...settings.speech_input,
        },
      };
    },
    async updateAuthSettings(settings) {
      systemSettings = {
        ...systemSettings,
        oidc: {
          ...systemSettings.oidc,
          ...settings.oidc,
        },
        ldap: {
          ...systemSettings.ldap,
          ...settings.ldap,
        },
        business_oauth: {
          ...systemSettings.business_oauth,
          ...settings.business_oauth,
        },
      };
    },
    async updatePublicPlatformSettings(settings) {
      systemSettings = {
        ...systemSettings,
        public_platform: {
          ...systemSettings.public_platform,
          ...settings.public_platform,
        },
      };
    },
    async updateApiManagementSettings(settings) {
      systemSettings = {
        ...systemSettings,
        api_management: {
          ...systemSettings.api_management,
          ...settings.api_management,
        },
      };
    },
    async updateSystemSettings(settings) {
      systemSettings = systemSettingsFromPayload(settings);
    },
    async hermesProfile() {
      return { ...hermesProfile };
    },
    async updateHermesProfile(profile) {
      hermesProfile = {
        soul_md: profile.soul_md,
      };
    },
    async listManagedSkills() {
      return Object.entries(managedSkills)
        .filter(([path]) => !hasHiddenManagedSkillSegment(path))
        .map(([path, content]) => ({ path, size: new Blob([content]).size }))
        .sort((left, right) => left.path.localeCompare(right.path));
    },
    async listManagedSkillTree() {
      return managedSkillTreeFromState();
    },
    async readManagedSkill(path) {
      if (options.readManagedSkill) {
        return options.readManagedSkill(path);
      }
      if (!(path in managedSkills)) {
        throw new Error("managed skill not found");
      }
      return { path, content: managedSkills[path] };
    },
    async saveManagedSkill(path, content) {
      if (options.saveManagedSkill) {
        const saved = await options.saveManagedSkill(path, content);
        managedSkills[saved.path] = saved.content;
        return saved;
      }
      managedSkills[path] = content;
      return { path, content };
    },
    async deleteManagedSkill(path) {
      if (options.deleteManagedSkill) {
        await options.deleteManagedSkill(path);
      }
      for (const skillPath of Object.keys(managedSkills)) {
        if (skillPath === path || skillPath.startsWith(`${path}/`)) {
          delete managedSkills[skillPath];
        }
      }
      managedSkillDirectories = new Set(
        Array.from(managedSkillDirectories).filter(
          (directory) => directory !== path && !directory.startsWith(`${path}/`),
        ),
      );
    },
    async createManagedSkillDirectory(path) {
      if (options.createManagedSkillDirectory) {
        await options.createManagedSkillDirectory(path);
      }
      managedSkillDirectories.add(path);
    },
    async uploadManagedSkills(files, targetPath) {
      if (files.some(isManagedSkillZipFile)) {
        throw new Error("managed skill zip uploads are not supported");
      }
      if (options.uploadManagedSkills) {
        const uploaded = await options.uploadManagedSkills(files, targetPath);
        for (const skill of uploaded) {
          managedSkills[skill.path] = "";
        }
        return uploaded;
      }
      const uploaded = files.map((file) => {
        const filePath = managedSkillFileName(file);
        const path = targetPath?.trim() ? `${targetPath.trim()}/${filePath}` : filePath;
        managedSkills[path] = "";
        return { path, size: file.size };
      });
      return uploaded.sort((left, right) => left.path.localeCompare(right.path));
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
      emitSessionEvent(sessionId, {
        type: "run_cleared",
        session_id: sessionId,
      });
      return null;
    },
    async clearHermesRun(_channelId, sessionId) {
      delete activeRunsBySessionId[sessionId];
      emitSessionEvent(sessionId, {
        type: "run_cleared",
        session_id: sessionId,
      });
    },
    async listSessionsPublic() {
      return sessions
        .slice()
        .sort(
          (left, right) =>
            Number(Boolean(right.is_home)) - Number(Boolean(left.is_home)) ||
            (right.updated_at ?? 0) - (left.updated_at ?? 0),
        )
        .map(({ id, title, is_home, deletable, created_at, updated_at, recycle_at }) => ({
          id,
          title,
          is_home,
          deletable,
          created_at,
          updated_at,
          recycle_at,
        }));
    },
    async createSessionPublic(kind = "agent", title) {
      const session = options.createSession
        ? await options.createSession("channel-1", kind, title)
        : await this.createSession("channel-1", kind, title);
      const publicSession = {
        id: session.id,
        title: session.title,
        created_at: session.created_at ?? Date.now(),
        updated_at: session.updated_at ?? session.created_at ?? Date.now(),
        recycle_at: session.recycle_at ?? Math.floor(Date.now() / 1000) + 24 * 60 * 60,
        public_url: `/public/sessions/${encodeURIComponent(session.id)}`,
      };
      publicPlatformSessions = [
        publicSession,
        ...publicPlatformSessions.filter((item) => item.id !== session.id),
      ];
      return {
        id: session.id,
        title: session.title,
        is_home: session.is_home,
        deletable: session.deletable,
        created_at: session.created_at,
        updated_at: session.updated_at,
        recycle_at: session.recycle_at,
      };
    },
    async deleteSessionPublic(sessionId) {
      if (sessions.find((session) => session.id === sessionId)?.deletable === false) {
        throw new Error("session is protected");
      }
      await this.deleteSession("channel-1", sessionId);
      publicPlatformSessions = publicPlatformSessions.filter((session) => session.id !== sessionId);
    },
    async appendSessionMessagePublic(sessionId, input) {
      if (input.role === "user") {
        const { message } = await this.createChannelRun("channel-1", sessionId, {
          content: input.content,
          attachments: input.attachments ?? [],
          clientMessageKey: input.clientMessageKey,
        });
        return message;
      }
      return this.appendSessionMessage("channel-1", sessionId, input);
    },
    async updateSessionMessagePublic(sessionId, messageId, input) {
      return this.updateSessionMessage("channel-1", sessionId, messageId, input);
    },
    async uploadSessionAttachmentsPublic(sessionId, files) {
      return this.uploadSessionAttachments("channel-1", sessionId, files);
    },
    async speechInputConfig() {
      return {
        enabled: false,
        runtime_available: false,
        max_duration_seconds: 60,
        sample_rate: 16000,
        model: "",
      };
    },
    openSpeechInputStream() {
      throw new Error("speech input is disabled");
    },
    subscribeSessionEventsPublic(sessionId, onEvent, onError) {
      return this.subscribeSessionEvents("channel-1", sessionId, onEvent, onError);
    },
    async stopSessionRunPublic(sessionId) {
      await this.stopHermesRun("channel-1", sessionId);
    },
  };
}
