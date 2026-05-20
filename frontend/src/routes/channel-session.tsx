import type { ApiClient, Channel, ChannelSession } from "../api/client";
import { SessionStream } from "../components/session-stream";
import { FormEvent, useEffect, useState } from "react";

type ChannelSessionRouteProps = {
  apiClient: ApiClient;
  channel: Channel | null;
};

export function ChannelSessionRoute({ apiClient, channel }: ChannelSessionRouteProps) {
  const [sessions, setSessions] = useState<ChannelSession[]>([]);
  const [title, setTitle] = useState("");
  const [kind, setKind] = useState<"chat" | "agent">("agent");
  const [prompt, setPrompt] = useState("");
  const [streamText, setStreamText] = useState("");
  const [error, setError] = useState<string | null>(null);

  async function refreshSessions() {
    if (!channel) {
      setSessions([]);
      return;
    }
    setSessions(await apiClient.listSessions(channel.id));
  }

  useEffect(() => {
    void refreshSessions();
  }, [channel?.id]);

  async function createSession(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!channel) {
      return;
    }
    await apiClient.createSession(channel.id, kind, title || undefined);
    setTitle("");
    await refreshSessions();
  }

  async function sendPrompt(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setError(null);
    setStreamText("");
    try {
      const text = await apiClient.sendHermesPrompt(prompt);
      setStreamText(text);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Hermes request failed");
    }
  }

  return (
    <section className="panel">
      <div className="panel-heading">
        <h2>Session</h2>
        <span>{channel?.name ?? "Select a channel"}</span>
      </div>
      <div className="session-grid">
        <div>
          <form className="inline-form" onSubmit={createSession}>
            <label>
              Kind
              <select value={kind} onChange={(event) => setKind(event.target.value as "chat" | "agent")}>
                <option value="agent">Agent</option>
                <option value="chat">Chat</option>
              </select>
            </label>
            <label>
              Title
              <input value={title} onChange={(event) => setTitle(event.target.value)} />
            </label>
            <button type="submit" disabled={!channel}>
              Create session
            </button>
          </form>
          <ul className="list compact-list">
            {sessions.map((session) => (
              <li key={session.id}>
                <strong>{session.title ?? session.kind}</strong>
                <span>{session.kind}</span>
              </li>
            ))}
          </ul>
        </div>
        <div>
          <form className="form" onSubmit={sendPrompt}>
            <label>
              Prompt
              <textarea value={prompt} onChange={(event) => setPrompt(event.target.value)} required />
            </label>
            {error ? <p className="error">{error}</p> : null}
            <button type="submit" disabled={!channel || !prompt.trim()}>
              Send to Hermes
            </button>
          </form>
          <SessionStream text={streamText || "Waiting for Hermes output"} />
        </div>
      </div>
    </section>
  );
}
