import type { ApiClient } from "./api/client";
import { createApiClient } from "./api/client";
import { Layout } from "./components/layout";
import { AdminRoute } from "./routes/admin";
import { ChannelSessionRoute } from "./routes/channel-session";
import { ChannelsRoute } from "./routes/channels";
import { LoginRoute } from "./routes/login";

type AppProps = {
  apiClient?: ApiClient;
};

export function App({ apiClient = createApiClient() }: AppProps) {
  return (
    <Layout>
      <LoginRoute />
      <AdminRoute apiClient={apiClient} />
      <ChannelsRoute apiClient={apiClient} />
      <ChannelSessionRoute apiClient={apiClient} />
    </Layout>
  );
}
