import type { ExampleHubClient, ExampleHubConfig, ExampleUserInfo } from "./hub-client";
import {
  clearStoredOAuthState,
  loadStoredOAuthState,
  saveStoredAccessToken,
  saveStoredOAuthState,
  saveStoredUserInfo,
  type StoredAccessToken,
} from "./storage";

export type ExampleAuthorizationResult = {
  accessToken: StoredAccessToken;
  userInfo: ExampleUserInfo;
};

export type ExampleAuthFlowOptions = {
  navigate?: (url: string) => void;
};

export type ExampleAuthFlow = ReturnType<typeof createExampleAuthFlow>;

const inflightOAuthCallbacks = new Map<string, Promise<ExampleAuthorizationResult>>();

function createOAuthState(): string {
  return `example-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

function buildAuthorizeUrl(config: ExampleHubConfig, state: string): string {
  const url = new URL(
    "/api/oauth/authorize",
    config.baseUrl.endsWith("/") ? config.baseUrl : `${config.baseUrl}/`,
  );
  url.searchParams.set("response_type", "code");
  url.searchParams.set("client_id", config.clientId);
  url.searchParams.set("redirect_uri", config.redirectUri);
  url.searchParams.set("scope", config.scopes);
  url.searchParams.set("state", state);
  return url.toString();
}

export function createExampleAuthFlow(
  client: Pick<ExampleHubClient, "exchangeAuthorizationCode" | "getUserInfo">,
  options: ExampleAuthFlowOptions = {},
) {
  return {
    beginAuthorization(config: ExampleHubConfig) {
      const state = createOAuthState();
      saveStoredOAuthState(state);
      const navigate = options.navigate ?? ((url: string) => window.location.assign(url));
      navigate(buildAuthorizeUrl(config, state));
    },
    async completeAuthorizationFromLocation(
      config: ExampleHubConfig,
    ): Promise<ExampleAuthorizationResult | null> {
      const currentUrl = new URL(window.location.href);
      const code = currentUrl.searchParams.get("code");
      const state = currentUrl.searchParams.get("state");
      const error = currentUrl.searchParams.get("error");
      if (error) {
        clearStoredOAuthState();
        window.history.replaceState({}, "", currentUrl.pathname);
        throw new Error(`OAuth 授权失败：${error}`);
      }
      if (!code) {
        return null;
      }

      const expectedState = loadStoredOAuthState();
      if (!state || !expectedState || expectedState !== state) {
        clearStoredOAuthState();
        window.history.replaceState({}, "", currentUrl.pathname);
        throw new Error("OAuth state 校验失败，请重新发起登录");
      }

      const callbackKey = `${config.redirectUri}::${code}::${state}`;
      const inflightCallback = inflightOAuthCallbacks.get(callbackKey);
      if (inflightCallback) {
        return inflightCallback;
      }

      const callbackPromise = (async () => {
        try {
          const token = await client.exchangeAuthorizationCode(config, code);
          const userInfo = await client.getUserInfo(config, token.access_token);
          saveStoredAccessToken(token);
          saveStoredUserInfo(userInfo);
          clearStoredOAuthState();
          window.history.replaceState({}, "", currentUrl.pathname);

          return {
            accessToken: {
              accessToken: token.access_token,
              tokenType: token.token_type,
              expiresIn: token.expires_in,
              scope: token.scope,
              createdAt: Date.now(),
            },
            userInfo,
          };
        } catch (error) {
          clearStoredOAuthState();
          window.history.replaceState({}, "", currentUrl.pathname);
          throw error;
        } finally {
          inflightOAuthCallbacks.delete(callbackKey);
        }
      })();

      inflightOAuthCallbacks.set(callbackKey, callbackPromise);
      return callbackPromise;
    },
  };
}
