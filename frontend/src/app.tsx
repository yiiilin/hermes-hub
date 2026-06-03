import type { ApiClient, User } from "./api/client";
import { createApiClient } from "./api/client";
import { Layout, type AppView } from "./components/layout";
import { I18nProvider, useI18n } from "./i18n";
import type { AdminSettingsTab, AppRoute, PersonalSettingsTab } from "./navigation";
import {
  appRoutesEqual,
  buildAppPath,
  defaultAdminSettingsTab,
  defaultPersonalSettingsTab,
  normalizeAppRouteForSessionState,
  parseAppPath,
  parseAppRoute,
  pushAppRoute,
  replaceAppRoute,
} from "./navigation";
import { ChannelSessionRoute } from "./routes/channel-session";
import { LoginRoute } from "./routes/login";
import { PersonalSettingsRoute } from "./routes/personal-settings";
import { ScheduledTasksRoute } from "./routes/scheduled-tasks";
import { lazy, Suspense, useCallback, useEffect, useMemo, useRef, useState } from "react";

const AdminRoute = lazy(() =>
  import("./routes/admin").then((module) => ({ default: module.AdminRoute })),
);

type AppProps = {
  apiClient?: ApiClient;
};

const HERMES_ACTIVITY_PREWARM_COOLDOWN_MS = 60_000;
const HERMES_ACTIVITY_HEARTBEAT_MS = 5 * 60_000;

export function App({ apiClient = createApiClient() }: AppProps) {
  return (
    <I18nProvider>
      <AppContent apiClient={apiClient} />
    </I18nProvider>
  );
}

function AppContent({ apiClient }: Required<AppProps>) {
  const { t } = useI18n();
  const [user, setUser] = useState<User | null>(null);
  const [loadingUser, setLoadingUser] = useState(true);
  const [publicPlatformEnabled, setPublicPlatformEnabled] = useState(false);
  const [currentRoute, setCurrentRoute] = useState<AppRoute>(() => parseAppRoute(window.location));
  useHermesActivityPrewarm(user, apiClient);

  async function fetchPublicPlatformEnabled() {
    try {
      const status = await apiClient.bootstrapStatus();
      return Boolean(status.public_platform_enabled);
    } catch {
      return false;
    }
  }

  useEffect(() => {
    let alive = true;
    void Promise.all([fetchPublicPlatformEnabled(), apiClient.me().catch(() => null)]).then(
      ([nextPublicPlatformEnabled, nextUser]) => {
        if (alive) {
          setPublicPlatformEnabled(nextPublicPlatformEnabled);
          setUser(nextUser);
          setLoadingUser(false);
        }
      },
    );

    return () => {
      alive = false;
    };
  }, [apiClient]);

  const navigateToRoute = useCallback((route: AppRoute, mode: "push" | "replace" = "push") => {
    if (mode === "replace") {
      replaceAppRoute(route);
    } else {
      pushAppRoute(route);
    }
    setCurrentRoute(parseAppRoute(window.location));
  }, []);

  useEffect(() => {
    function handlePopState() {
      setCurrentRoute(parseAppRoute(window.location));
    }

    window.addEventListener("popstate", handlePopState);
    return () => window.removeEventListener("popstate", handlePopState);
  }, []);

  const normalizedRoute = useMemo(
    () =>
      loadingUser
        ? currentRoute
        : normalizeAppRouteForSessionState({
            publicPlatformEnabled,
            route: currentRoute,
            user,
          }),
    [currentRoute, loadingUser, publicPlatformEnabled, user],
  );
  const browserPathAtRender = `${window.location.pathname}${window.location.search}${window.location.hash}`;

  useEffect(() => {
    if (loadingUser || shouldPreserveInviteUrl(normalizedRoute)) {
      return;
    }
    const normalizedPath = buildAppPath(normalizedRoute);
    if (browserPathAtRender === normalizedPath && appRoutesEqual(currentRoute, normalizedRoute)) {
      return;
    }
    replaceAppRoute(normalizedRoute);
    setCurrentRoute(parseAppRoute(window.location));
  }, [browserPathAtRender, currentRoute, loadingUser, normalizedRoute]);

  async function logout() {
    await apiClient.logout();
    setUser(null);
    setLoadingUser(true);
    const nextPublicPlatformEnabled = await fetchPublicPlatformEnabled();
    setPublicPlatformEnabled(nextPublicPlatformEnabled);
    navigateToRoute(
      nextPublicPlatformEnabled
        ? { name: "public-chat", sessionId: null }
        : { name: "login", next: null },
      "replace",
    );
    setLoadingUser(false);
  }

  if (loadingUser) {
    return <main className="auth-shell">{t("common.loading")}</main>;
  }

  if (!user) {
    if (normalizedRoute.name === "public-chat" && publicPlatformEnabled) {
      return (
        <Layout
          key="public"
          user={null}
          activeView="chat"
          onNavigate={(view) => navigateToRoute(routeForLayoutView(view, null), "push")}
          onLogin={() => navigateToRoute({ name: "login", next: null }, "push")}
        >
          <ChannelSessionRoute
            key="public"
            active
            apiClient={apiClient}
            onSessionRouteChange={(sessionId, mode) =>
              navigateToRoute({ name: "public-chat", sessionId }, mode)
            }
            publicMode
            routeSessionId={normalizedRoute.sessionId ?? null}
          />
        </Layout>
      );
    }

    return (
      <LoginRoute
        apiClient={apiClient}
        onAuthenticated={(nextUser) => {
          setUser(nextUser);
          const nextRoute =
            normalizedRoute.name === "login" && normalizedRoute.next
              ? parseAppPath(normalizedRoute.next)
              : ({ name: "chat", sessionId: null } satisfies AppRoute);
          navigateToRoute(
            normalizeAppRouteForSessionState({
              publicPlatformEnabled,
              route: nextRoute,
              user: nextUser,
            }),
            "replace",
          );
        }}
        onBackToPublicPlatform={
          publicPlatformEnabled
            ? () => navigateToRoute({ name: "public-chat", sessionId: null }, "replace")
            : undefined
        }
      />
    );
  }

  const activeView = appViewForRoute(normalizedRoute);
  const activeAdminTab =
    normalizedRoute.name === "settings" ? normalizedRoute.tab : defaultAdminSettingsTab;
  const activePersonalTab =
    normalizedRoute.name === "personal" ? normalizedRoute.tab : defaultPersonalSettingsTab;

  return (
    <Layout
      key={`user:${user.id}`}
      user={user}
      activeView={activeView}
      onNavigate={(view) => navigateToRoute(routeForLayoutView(view, user), "push")}
      onLogout={logout}
    >
      <ChannelSessionRoute
        key={`user:${user.id}`}
        active={activeView === "chat"}
        apiClient={apiClient}
        onSessionRouteChange={(sessionId, mode) =>
          navigateToRoute({ name: "chat", sessionId }, mode)
        }
        publicMode={false}
        routeSessionId={normalizedRoute.name === "chat" ? (normalizedRoute.sessionId ?? null) : null}
      />
      {user.role === "admin" && activeView === "admin-settings" ? (
        <Suspense fallback={<main className="auth-shell">{t("common.loading")}</main>}>
          <AdminRoute
            activeTab={activeAdminTab}
            apiClient={apiClient}
            currentUser={user}
            onTabChange={(tab: AdminSettingsTab) =>
              navigateToRoute({ name: "settings", tab }, "push")
            }
          />
        </Suspense>
      ) : null}
      <ScheduledTasksRoute
        active={activeView === "scheduled-tasks"}
        apiClient={apiClient}
      />
      <PersonalSettingsRoute
        active={activeView === "personal-settings"}
        activeTab={activePersonalTab}
        apiClient={apiClient}
        onTabChange={(tab: PersonalSettingsTab) =>
          navigateToRoute({ name: "personal", tab }, "push")
        }
      />
    </Layout>
  );
}

function shouldPreserveInviteUrl(route: AppRoute) {
  if (route.name !== "login") {
    return false;
  }
  const params = new URLSearchParams(window.location.search);
  return params.has("invite") || params.has("invite_token");
}

function appViewForRoute(route: AppRoute): AppView {
  switch (route.name) {
    case "settings":
      return "admin-settings";
    case "scheduled-tasks":
      return "scheduled-tasks";
    case "personal":
      return "personal-settings";
    default:
      return "chat";
  }
}

function routeForLayoutView(view: AppView, user: User | null): AppRoute {
  switch (view) {
    case "admin-settings":
      return user?.role === "admin"
        ? { name: "settings", tab: defaultAdminSettingsTab }
        : { name: "chat", sessionId: null };
    case "scheduled-tasks":
      return { name: "scheduled-tasks" };
    case "personal-settings":
      return { name: "personal", tab: defaultPersonalSettingsTab };
    case "login":
      return { name: "login", next: null };
    case "chat":
      return user ? { name: "chat", sessionId: null } : { name: "public-chat", sessionId: null };
  }
}

function useHermesActivityPrewarm(user: User | null, apiClient: ApiClient) {
  const lastPrewarmAtRef = useRef(0);
  const prewarmInFlightRef = useRef(false);

  const prewarmHermes = useCallback(() => {
    if (!user || prewarmInFlightRef.current || document.visibilityState === "hidden") {
      return;
    }

    const now = Date.now();
    if (now - lastPrewarmAtRef.current < HERMES_ACTIVITY_PREWARM_COOLDOWN_MS) {
      return;
    }

    lastPrewarmAtRef.current = now;
    prewarmInFlightRef.current = true;
    // 这里故意静默失败：真正发送消息时仍会返回明确错误，预热不应该打断用户当前页面。
    void apiClient
      .ensureHermes()
      .catch(() => undefined)
      .finally(() => {
        prewarmInFlightRef.current = false;
      });
  }, [apiClient, user]);

  useEffect(() => {
    if (!user) {
      lastPrewarmAtRef.current = 0;
      prewarmInFlightRef.current = false;
      return undefined;
    }

    prewarmHermes();

    const prewarmWhenVisible = () => {
      if (document.visibilityState === "visible") {
        prewarmHermes();
      }
    };
    const activityEvents: Array<keyof WindowEventMap> = ["focus", "keydown", "pointerdown"];

    document.addEventListener("visibilitychange", prewarmWhenVisible);
    for (const eventName of activityEvents) {
      window.addEventListener(eventName, prewarmHermes);
    }
    const heartbeatId = window.setInterval(prewarmHermes, HERMES_ACTIVITY_HEARTBEAT_MS);

    return () => {
      document.removeEventListener("visibilitychange", prewarmWhenVisible);
      for (const eventName of activityEvents) {
        window.removeEventListener(eventName, prewarmHermes);
      }
      window.clearInterval(heartbeatId);
    };
  }, [prewarmHermes, user]);
}
