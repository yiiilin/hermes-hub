import type { ApiClient, HermesInstance, Invite, ModelConfig, User } from "../api/client";
import { FormEvent, useEffect, useState } from "react";

type AdminRouteProps = {
  apiClient: ApiClient;
  currentUser: User;
};

const defaultInviteHours = 24;

export function AdminRoute({ apiClient, currentUser }: AdminRouteProps) {
  const [users, setUsers] = useState<User[]>([]);
  const [invites, setInvites] = useState<Invite[]>([]);
  const [instances, setInstances] = useState<HermesInstance[]>([]);
  const [modelConfig, setModelConfig] = useState<ModelConfig | null>(null);
  const [inviteHours, setInviteHours] = useState(defaultInviteHours);
  const [inviteMaxUses, setInviteMaxUses] = useState(1);
  const [lastInviteLink, setLastInviteLink] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setError(null);
    try {
      const [nextUsers, nextInvites, nextInstances, nextModel] = await Promise.all([
        apiClient.listUsers(),
        apiClient.listInvites(),
        apiClient.listHermesInstances(),
        apiClient.modelConfig(),
      ]);
      setUsers(nextUsers);
      setInvites(nextInvites);
      setInstances(nextInstances);
      setModelConfig(nextModel);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Admin data could not be loaded");
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function createInvite(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    const expiresAt = Math.floor(Date.now() / 1000) + inviteHours * 60 * 60;
    const created = await apiClient.createInvite({
      expires_at: expiresAt,
      max_uses: inviteMaxUses,
    });
    setLastInviteLink(`${window.location.origin}/?invite=${created.token}`);
    await refresh();
  }

  async function saveModel(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!modelConfig) {
      return;
    }
    await apiClient.updateModelConfig(modelConfig);
    await refresh();
  }

  async function toggleUser(user: User) {
    if (user.id === currentUser.id) {
      return;
    }
    if (user.status === "active") {
      await apiClient.disableUser(user.id);
    } else {
      await apiClient.enableUser(user.id);
    }
    await refresh();
  }

  async function controlInstance(action: "start" | "stop" | "rebuild", instance: HermesInstance) {
    if (action === "start") {
      await apiClient.startHermesInstance(instance.user_id);
    } else if (action === "stop") {
      await apiClient.stopHermesInstance(instance.user_id);
    } else {
      await apiClient.rebuildHermesInstance(instance.user_id);
    }
    await refresh();
  }

  return (
    <section className="grid-section" id="admin">
      <div className="panel">
        <div className="panel-heading">
          <h2>Users</h2>
          <button type="button" className="secondary" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        <table>
          <thead>
            <tr>
              <th>Email</th>
              <th>Role</th>
              <th>Status</th>
              <th>Action</th>
            </tr>
          </thead>
          <tbody>
            {users.map((user) => (
              <tr key={user.id}>
                <td>{user.email}</td>
                <td>{user.role}</td>
                <td>{user.status}</td>
                <td>
                  <button
                    type="button"
                    className="secondary"
                    disabled={user.id === currentUser.id}
                    onClick={() => void toggleUser(user)}
                  >
                    {user.status === "active" ? "Disable" : "Enable"}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="panel">
        <h2>Invites</h2>
        <form className="inline-form" onSubmit={createInvite}>
          <label>
            Hours
            <input
              type="number"
              min={1}
              value={inviteHours}
              onChange={(event) => setInviteHours(Number(event.target.value))}
              required
            />
          </label>
          <label>
            Uses
            <input
              type="number"
              min={1}
              value={inviteMaxUses}
              onChange={(event) => setInviteMaxUses(Number(event.target.value))}
              required
            />
          </label>
          <button type="submit">Create invite</button>
        </form>
        {lastInviteLink ? <p className="copy-line">{lastInviteLink}</p> : null}
        <ul className="list compact-list">
          {invites.map((invite) => (
            <li key={invite.id}>
              <strong>{invite.status}</strong>
              <span>
                {invite.used_count}/{invite.max_uses} used · expires{" "}
                {new Date(invite.expires_at * 1000).toLocaleString()}
              </span>
              {invite.status === "pending" ? (
                <button
                  type="button"
                  className="secondary"
                  onClick={() => void apiClient.revokeInvite(invite.id).then(refresh)}
                >
                  Revoke
                </button>
              ) : null}
            </li>
          ))}
        </ul>
      </div>

      <div className="panel">
        <h2>Model configuration</h2>
        {modelConfig ? (
          <form className="form" onSubmit={saveModel}>
            <label>
              Provider
              <input
                value={modelConfig.provider_name}
                onChange={(event) =>
                  setModelConfig({ ...modelConfig, provider_name: event.target.value })
                }
              />
            </label>
            <label>
              Base URL
              <input
                value={modelConfig.provider_base_url}
                onChange={(event) =>
                  setModelConfig({ ...modelConfig, provider_base_url: event.target.value })
                }
              />
            </label>
            <label>
              API key
              <input
                type="password"
                value={modelConfig.provider_api_key ?? ""}
                onChange={(event) =>
                  setModelConfig({ ...modelConfig, provider_api_key: event.target.value })
                }
              />
            </label>
            <label>
              Default model
              <input
                value={modelConfig.default_model}
                onChange={(event) =>
                  setModelConfig({ ...modelConfig, default_model: event.target.value })
                }
              />
            </label>
            <label>
              Allowed models
              <input
                value={modelConfig.allowed_models.join(",")}
                onChange={(event) =>
                  setModelConfig({
                    ...modelConfig,
                    allowed_models: event.target.value
                      .split(",")
                      .map((model) => model.trim())
                      .filter(Boolean),
                  })
                }
              />
            </label>
            <label>
              Timeout seconds
              <input
                type="number"
                min={1}
                value={modelConfig.request_timeout_seconds}
                onChange={(event) =>
                  setModelConfig({
                    ...modelConfig,
                    request_timeout_seconds: Number(event.target.value),
                  })
                }
              />
            </label>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={modelConfig.allow_streaming}
                onChange={(event) =>
                  setModelConfig({ ...modelConfig, allow_streaming: event.target.checked })
                }
              />
              Streaming
            </label>
            <button type="submit">Save model</button>
          </form>
        ) : null}
      </div>

      <div className="panel">
        <h2>Hermes instances</h2>
        <ul className="list compact-list">
          {instances.map((instance) => (
            <li key={instance.id}>
              <strong>{instance.name ?? instance.user_id}</strong>
              <span>
                {instance.kind} · {instance.status}
              </span>
              {instance.kind === "managed_docker" ? (
                <div className="button-row">
                  <button type="button" className="secondary" onClick={() => void controlInstance("start", instance)}>
                    Start
                  </button>
                  <button type="button" className="secondary" onClick={() => void controlInstance("stop", instance)}>
                    Stop
                  </button>
                  <button type="button" className="secondary" onClick={() => void controlInstance("rebuild", instance)}>
                    Rebuild
                  </button>
                </div>
              ) : null}
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}
