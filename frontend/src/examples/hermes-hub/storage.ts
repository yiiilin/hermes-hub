import type { ExampleHubConfig, ExampleOAuthToken, ExampleUserInfo } from "./hub-client";

export const EXAMPLE_STORAGE_KEYS = {
  config: "hermes-hub-example.config",
  accessToken: "hermes-hub-example.access-token",
  oauthState: "hermes-hub-example.oauth-state",
  pendingToolResults: "hermes-hub-example.pending-tool-results",
  selectedSessionId: "hermes-hub-example.selected-session-id",
  userInfo: "hermes-hub-example.user-info",
} as const;

export type StoredAccessToken = {
  accessToken: string;
  tokenType: string;
  expiresIn: number;
  scope: string;
  createdAt: number;
};

export type StoredPendingToolResult = {
  requestId: string;
  sessionId: string;
  toolName: string;
  resultStatus: "completed" | "failed";
  summary: string;
  resultText: string;
  argumentsPreview: string;
  createdAt: number;
};

function readJson<T>(key: string): T | null {
  const raw = localStorage.getItem(key);
  if (!raw) {
    return null;
  }
  try {
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
}

function writeJson(key: string, value: unknown) {
  localStorage.setItem(key, JSON.stringify(value));
}

export function loadStoredConfig(): ExampleHubConfig | null {
  return readJson<ExampleHubConfig>(EXAMPLE_STORAGE_KEYS.config);
}

export function saveStoredConfig(config: ExampleHubConfig) {
  writeJson(EXAMPLE_STORAGE_KEYS.config, config);
}

export function clearStoredConfig() {
  localStorage.removeItem(EXAMPLE_STORAGE_KEYS.config);
}

export function loadStoredAccessToken(): StoredAccessToken | null {
  return readJson<StoredAccessToken>(EXAMPLE_STORAGE_KEYS.accessToken);
}

export function saveStoredAccessToken(token: ExampleOAuthToken) {
  writeJson(EXAMPLE_STORAGE_KEYS.accessToken, {
    accessToken: token.access_token,
    tokenType: token.token_type,
    expiresIn: token.expires_in,
    scope: token.scope,
    createdAt: Date.now(),
  } satisfies StoredAccessToken);
}

export function clearStoredAccessToken() {
  localStorage.removeItem(EXAMPLE_STORAGE_KEYS.accessToken);
}

export function loadStoredOAuthState(): string | null {
  return localStorage.getItem(EXAMPLE_STORAGE_KEYS.oauthState);
}

export function saveStoredOAuthState(state: string) {
  localStorage.setItem(EXAMPLE_STORAGE_KEYS.oauthState, state);
}

export function clearStoredOAuthState() {
  localStorage.removeItem(EXAMPLE_STORAGE_KEYS.oauthState);
}

function storedPendingToolResultKey(sessionId: string, requestId: string): string {
  return `${sessionId}::${requestId}`;
}

export function loadStoredPendingToolResults(): Record<string, StoredPendingToolResult> {
  return readJson<Record<string, StoredPendingToolResult>>(EXAMPLE_STORAGE_KEYS.pendingToolResults) ?? {};
}

export function loadStoredPendingToolResult(
  sessionId: string,
  requestId: string,
): StoredPendingToolResult | null {
  return loadStoredPendingToolResults()[storedPendingToolResultKey(sessionId, requestId)] ?? null;
}

export function saveStoredPendingToolResult(result: StoredPendingToolResult) {
  const nextResults = {
    ...loadStoredPendingToolResults(),
    [storedPendingToolResultKey(result.sessionId, result.requestId)]: result,
  };
  writeJson(EXAMPLE_STORAGE_KEYS.pendingToolResults, nextResults);
}

export function clearStoredPendingToolResult(sessionId: string, requestId: string) {
  const nextResults = { ...loadStoredPendingToolResults() };
  delete nextResults[storedPendingToolResultKey(sessionId, requestId)];
  if (Object.keys(nextResults).length === 0) {
    localStorage.removeItem(EXAMPLE_STORAGE_KEYS.pendingToolResults);
    return;
  }
  writeJson(EXAMPLE_STORAGE_KEYS.pendingToolResults, nextResults);
}

export function clearStoredPendingToolResultsForSession(sessionId: string) {
  const nextResults = Object.fromEntries(
    Object.entries(loadStoredPendingToolResults()).filter(
      ([, value]) => value.sessionId !== sessionId,
    ),
  );
  if (Object.keys(nextResults).length === 0) {
    localStorage.removeItem(EXAMPLE_STORAGE_KEYS.pendingToolResults);
    return;
  }
  writeJson(EXAMPLE_STORAGE_KEYS.pendingToolResults, nextResults);
}

export function clearStoredPendingToolResults() {
  localStorage.removeItem(EXAMPLE_STORAGE_KEYS.pendingToolResults);
}

export function loadStoredSelectedSessionId(): string | null {
  return localStorage.getItem(EXAMPLE_STORAGE_KEYS.selectedSessionId);
}

export function saveStoredSelectedSessionId(sessionId: string | null) {
  if (!sessionId) {
    localStorage.removeItem(EXAMPLE_STORAGE_KEYS.selectedSessionId);
    return;
  }
  localStorage.setItem(EXAMPLE_STORAGE_KEYS.selectedSessionId, sessionId);
}

export function loadStoredUserInfo(): ExampleUserInfo | null {
  return readJson<ExampleUserInfo>(EXAMPLE_STORAGE_KEYS.userInfo);
}

export function saveStoredUserInfo(userInfo: ExampleUserInfo) {
  writeJson(EXAMPLE_STORAGE_KEYS.userInfo, userInfo);
}

export function clearStoredUserInfo() {
  localStorage.removeItem(EXAMPLE_STORAGE_KEYS.userInfo);
}

export function isStoredAccessTokenExpired(
  token: StoredAccessToken,
  now = Date.now(),
): boolean {
  const expiresAt = token.createdAt + token.expiresIn * 1000;
  return now >= expiresAt;
}
