import type { ApiClient, User } from "./api/client";
import { createApiClient } from "./api/client";
import { Layout, type AppView } from "./components/layout";
import { I18nProvider, useI18n } from "./i18n";
import { ChannelSessionRoute } from "./routes/channel-session";
import { LoginRoute } from "./routes/login";
import { PersonalSettingsRoute } from "./routes/personal-settings";
import { ScheduledTasksRoute } from "./routes/scheduled-tasks";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";

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
  const [activeView, setActiveView] = useState<AppView>("chat");
  useHermesActivityPrewarm(user, apiClient);

  useEffect(() => {
    let alive = true;
    void apiClient
      .me()
      .then((nextUser) => {
        if (alive) {
          setUser(nextUser);
        }
      })
      .catch(() => {
        if (alive) {
          setUser(null);
        }
      })
      .finally(() => {
        if (alive) {
          setLoadingUser(false);
        }
      });

    return () => {
      alive = false;
    };
  }, [apiClient]);

  async function logout() {
    await apiClient.logout();
    setUser(null);
    setActiveView("chat");
  }

  if (loadingUser) {
    return <main className="auth-shell">{t("common.loading")}</main>;
  }

  if (!user) {
    return <LoginRoute apiClient={apiClient} onAuthenticated={setUser} />;
  }

  return (
    <Layout
      user={user}
      activeView={activeView}
      onNavigate={setActiveView}
      onLogout={logout}
    >
      <ChannelSessionRoute
        active={activeView === "chat"}
        apiClient={apiClient}
        onOpenChat={() => setActiveView("chat")}
      />
      {user.role === "admin" && activeView === "admin-settings" ? (
        <Suspense fallback={<main className="auth-shell">{t("common.loading")}</main>}>
          <AdminRoute apiClient={apiClient} currentUser={user} />
        </Suspense>
      ) : null}
      <ScheduledTasksRoute
        active={activeView === "scheduled-tasks"}
        apiClient={apiClient}
      />
      <PersonalSettingsRoute
        active={activeView === "personal-settings"}
        apiClient={apiClient}
      />
    </Layout>
  );
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
