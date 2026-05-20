import type { ApiClient, User } from "./api/client";
import { createApiClient } from "./api/client";
import { Layout, type AppView } from "./components/layout";
import { AdminRoute } from "./routes/admin";
import { ChannelSessionRoute } from "./routes/channel-session";
import { LoginRoute } from "./routes/login";
import { useEffect, useState } from "react";

type AppProps = {
  apiClient?: ApiClient;
};

export function App({ apiClient = createApiClient() }: AppProps) {
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
    return <main className="auth-shell">Loading</main>;
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
      {activeView === "chat" ? <ChannelSessionRoute apiClient={apiClient} /> : null}
      {user.role === "admin" && activeView === "admin-users" ? (
        <AdminRoute apiClient={apiClient} currentUser={user} section="users" />
      ) : null}
      {user.role === "admin" && activeView === "admin-models" ? (
        <AdminRoute apiClient={apiClient} currentUser={user} section="models" />
      ) : null}
      {user.role === "admin" && activeView === "admin-hermes" ? (
        <AdminRoute apiClient={apiClient} currentUser={user} section="hermes" />
      ) : null}
    </Layout>
  );
}
