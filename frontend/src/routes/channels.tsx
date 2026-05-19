import type { ApiClient, Channel, HermesInstance } from "../api/client";
import { useEffect, useState } from "react";

type ChannelsRouteProps = {
  apiClient: ApiClient;
};

export function ChannelsRoute({ apiClient }: ChannelsRouteProps) {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [instance, setInstance] = useState<HermesInstance | null>(null);

  useEffect(() => {
    void apiClient.listChannels().then(setChannels);
    void apiClient.workspaceStatus().then(setInstance);
  }, [apiClient]);

  return (
    <section className="grid-section" id="workspace">
      <div className="panel">
        <h2>Channels</h2>
        <ul className="list">
          {channels.map((channel) => (
            <li key={channel.id}>
              <strong>{channel.name}</strong>
              <span>{channel.description}</span>
            </li>
          ))}
        </ul>
      </div>
      <div className="panel">
        <h2>Hermes instance</h2>
        <dl>
          <dt>Status</dt>
          <dd>{instance?.status ?? "not provisioned"}</dd>
          <dt>Kind</dt>
          <dd>{instance?.kind ?? "none"}</dd>
        </dl>
      </div>
    </section>
  );
}
