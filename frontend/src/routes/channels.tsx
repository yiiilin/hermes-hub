import type { ApiClient, Channel, HermesInstance } from "../api/client";
import { FormEvent, useEffect, useState } from "react";

type ChannelsRouteProps = {
  apiClient: ApiClient;
  selectedChannelId: string | null;
  onSelectChannel: (channel: Channel) => void;
};

export function ChannelsRoute({
  apiClient,
  selectedChannelId,
  onSelectChannel,
}: ChannelsRouteProps) {
  const [channels, setChannels] = useState<Channel[]>([]);
  const [instance, setInstance] = useState<HermesInstance | null>(null);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setError(null);
    try {
      const [nextChannels, nextInstance] = await Promise.all([
        apiClient.listChannels(),
        apiClient.workspaceStatus(),
      ]);
      setChannels(nextChannels);
      setInstance(nextInstance);
      if (!selectedChannelId && nextChannels[0]) {
        onSelectChannel(nextChannels[0]);
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Workspace data could not be loaded");
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function createChannel(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const channel = await apiClient.createChannel(name, description || undefined);
    setName("");
    setDescription("");
    await refresh();
    onSelectChannel(channel);
  }

  async function ensureHermes() {
    const ensured = await apiClient.ensureHermes();
    setInstance(ensured);
  }

  return (
    <section className="grid-section" id="workspace">
      <div className="panel">
        <div className="panel-heading">
          <h2>Channels</h2>
          <button type="button" className="secondary" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        <form className="inline-form" onSubmit={createChannel}>
          <label>
            Name
            <input value={name} onChange={(event) => setName(event.target.value)} required />
          </label>
          <label>
            Description
            <input value={description} onChange={(event) => setDescription(event.target.value)} />
          </label>
          <button type="submit">Create channel</button>
        </form>
        <ul className="list">
          {channels.map((channel) => (
            <li
              key={channel.id}
              className={channel.id === selectedChannelId ? "selected" : ""}
            >
              <button type="button" className="list-button" onClick={() => onSelectChannel(channel)}>
                <strong>{channel.name}</strong>
                <span>{channel.description || "No description"}</span>
              </button>
            </li>
          ))}
        </ul>
      </div>
      <div className="panel">
        <div className="panel-heading">
          <h2>Hermes instance</h2>
          <button type="button" onClick={() => void ensureHermes()}>
            Ensure Hermes
          </button>
        </div>
        <dl>
          <dt>Status</dt>
          <dd>{instance?.status ?? "not provisioned"}</dd>
          <dt>Kind</dt>
          <dd>{instance?.kind ?? "none"}</dd>
          <dt>Base URL</dt>
          <dd>{instance?.base_url ?? "none"}</dd>
        </dl>
      </div>
    </section>
  );
}
