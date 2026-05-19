import type { ApiClient, ChannelSession } from "../api/client";
import { useEffect, useState } from "react";

type ChannelSessionRouteProps = {
  apiClient: ApiClient;
};

export function ChannelSessionRoute({ apiClient }: ChannelSessionRouteProps) {
  const [sessions, setSessions] = useState<ChannelSession[]>([]);

  useEffect(() => {
    void apiClient.listSessions("channel-1").then(setSessions);
  }, [apiClient]);

  return (
    <section className="panel">
      <h2>Session</h2>
      <div className="session-surface">
        {sessions.map((session) => (
          <article key={session.id}>
            <strong>{session.title ?? session.kind}</strong>
            <p>Streaming output appears here when Hermes returns events.</p>
          </article>
        ))}
      </div>
    </section>
  );
}
