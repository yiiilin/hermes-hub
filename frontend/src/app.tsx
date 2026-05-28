import type { ApiClient, User } from "./api/client";
import { createApiClient } from "./api/client";
import { Layout, type AppView } from "./components/layout";
import { I18nProvider, useI18n } from "./i18n";
import { ChannelSessionRoute } from "./routes/channel-session";
import { LoginRoute } from "./routes/login";
import { ScheduledTasksRoute } from "./routes/scheduled-tasks";
import { lazy, Suspense, useEffect, useState } from "react";

const AdminRoute = lazy(() =>
  import("./routes/admin").then((module) => ({ default: module.AdminRoute })),
);

type AppProps = {
  apiClient?: ApiClient;
};

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
    </Layout>
  );
}
