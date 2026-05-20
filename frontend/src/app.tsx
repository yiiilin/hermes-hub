import type { ApiClient, Channel, User } from "./api/client";
import { createApiClient } from "./api/client";
import { Layout } from "./components/layout";
import { AdminRoute } from "./routes/admin";
import { ChannelSessionRoute } from "./routes/channel-session";
import { ChannelsRoute } from "./routes/channels";
import { LoginRoute } from "./routes/login";
import { useEffect, useState } from "react";

type AppProps = {
  apiClient?: ApiClient;
};

export function App({ apiClient = createApiClient() }: AppProps) {
  const [user, setUser] = useState<User | null>(null);
  const [selectedChannel, setSelectedChannel] = useState<Channel | null>(null);
  const [loadingUser, setLoadingUser] = useState(true);

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
    setSelectedChannel(null);
  }

  if (loadingUser) {
    return (
      <Layout user={null}>
        <section className="panel">Loading</section>
      </Layout>
    );
  }

  if (!user) {
    return (
      <Layout user={null}>
        <LoginRoute apiClient={apiClient} onAuthenticated={setUser} />
      </Layout>
    );
  }

  return (
    <Layout user={user} onLogout={logout}>
      {user.role === "admin" ? <AdminRoute apiClient={apiClient} currentUser={user} /> : null}
      <ChannelsRoute
        apiClient={apiClient}
        selectedChannelId={selectedChannel?.id ?? null}
        onSelectChannel={setSelectedChannel}
      />
      <ChannelSessionRoute apiClient={apiClient} channel={selectedChannel} />
    </Layout>
  );
}
