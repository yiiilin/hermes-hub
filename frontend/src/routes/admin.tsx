import type {
  ApiClient,
  HermesInstance,
  Invite,
  ModelConfig,
  ModelConfigKind,
  User,
} from "../api/client";
import { FormEvent, useEffect, useMemo, useState } from "react";

type AdminSection = "users" | "models" | "hermes";

type AdminRouteProps = {
  apiClient: ApiClient;
  currentUser: User;
  section: AdminSection;
};

const defaultInviteHours = 24;
const modelLabels: Record<ModelConfigKind, string> = {
  llm: "大模型",
  image: "图片生成模型",
  title: "标题生成模型",
};

export function AdminRoute({ apiClient, currentUser, section }: AdminRouteProps) {
  const [users, setUsers] = useState<User[]>([]);
  const [invites, setInvites] = useState<Invite[]>([]);
  const [instances, setInstances] = useState<HermesInstance[]>([]);
  const [modelConfigs, setModelConfigs] = useState<ModelConfig[]>([]);
  const [inviteHours, setInviteHours] = useState(defaultInviteHours);
  const [inviteMaxUses, setInviteMaxUses] = useState(1);
  const [lastInviteLink, setLastInviteLink] = useState<string | null>(null);
  const [requiredModelsReady, setRequiredModelsReady] = useState(false);
  const [missingRequiredModels, setMissingRequiredModels] = useState<ModelConfigKind[]>([]);
  const [modelTestMessages, setModelTestMessages] = useState<
    Partial<Record<ModelConfigKind, string>>
  >({});
  const [testingModel, setTestingModel] = useState<ModelConfigKind | null>(null);
  const [error, setError] = useState<string | null>(null);

  const instancesByUserId = useMemo(
    () => new Map(instances.map((instance) => [instance.user_id, instance])),
    [instances],
  );

  async function refresh() {
    setError(null);
    try {
      const [nextUsers, nextInvites, nextInstances, nextModelStatus] = await Promise.all([
        apiClient.listUsers(),
        apiClient.listInvites(),
        apiClient.listHermesInstances(),
        apiClient.modelConfigStatus(),
      ]);
      setUsers(nextUsers);
      setInvites(nextInvites);
      setInstances(nextInstances);
      setModelConfigs(nextModelStatus.model_configs);
      setRequiredModelsReady(nextModelStatus.required_models_ready);
      setMissingRequiredModels(nextModelStatus.missing_required_model_config_kinds);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Admin data could not be loaded");
    }
  }

  useEffect(() => {
    void refresh();
  }, []);

  async function createInvite(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!requiredModelsReady) {
      setError(modelGateMessage);
      return;
    }
    const expiresAt = Math.floor(Date.now() / 1000) + inviteHours * 60 * 60;
    const created = await apiClient.createInvite({
      expires_at: expiresAt,
      max_uses: inviteMaxUses,
    });
    setLastInviteLink(`${window.location.origin}/?invite=${created.token}`);
    await refresh();
  }

  async function saveModels(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await apiClient.updateModelConfigs(modelConfigs);
    await refresh();
  }

  function updateModel(kind: ModelConfigKind, patch: Partial<ModelConfig>) {
    setModelConfigs((configs) =>
      configs.map((config) =>
        config.config_kind === kind ? { ...config, ...patch } : config,
      ),
    );
  }

  async function testModel(config: ModelConfig) {
    setTestingModel(config.config_kind);
    setModelTestMessages((messages) => ({
      ...messages,
      [config.config_kind]: "Testing...",
    }));
    try {
      const result = await apiClient.testModelConfig(config);
      setModelTestMessages((messages) => ({
        ...messages,
        [config.config_kind]: result.ok
          ? result.message
          : `HTTP ${result.status_code}: ${result.message}`,
      }));
    } catch (cause) {
      setModelTestMessages((messages) => ({
        ...messages,
        [config.config_kind]:
          cause instanceof Error ? cause.message : "Model test failed",
      }));
    } finally {
      setTestingModel(null);
    }
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
    if (action !== "stop" && !requiredModelsReady) {
      setError(modelGateMessage);
      return;
    }
    if (action === "start") {
      await apiClient.startHermesInstance(instance.user_id);
    } else if (action === "stop") {
      await apiClient.stopHermesInstance(instance.user_id);
    } else {
      await apiClient.rebuildHermesInstance(instance.user_id);
    }
    await refresh();
  }

  async function createManagedHermes(userId: string) {
    if (!requiredModelsReady) {
      setError(modelGateMessage);
      return;
    }
    await apiClient.createHermesInstance(userId);
    await refresh();
  }

  const missingRequiredModelNames =
    missingRequiredModels.length > 0
      ? missingRequiredModels.map((kind) => modelLabels[kind]).join("、")
      : "大模型、标题生成模型";
  const modelGateMessage = `请先在模型配置管理中保存可用的${missingRequiredModelNames}。`;

  if (section === "models") {
    return (
      <section className="admin-page" id="admin-models">
        <form className="admin-page" onSubmit={(event) => void saveModels(event)}>
          <div className="panel-heading">
            <h1>模型配置管理</h1>
            <div className="button-row">
              <button type="button" className="secondary" onClick={() => void refresh()}>
                Refresh
              </button>
              <button type="submit">Save</button>
            </div>
          </div>
          {error ? <p className="error">{error}</p> : null}
          <div className="model-config-grid">
            {modelConfigs.map((config) => (
              <section className="panel" key={config.config_kind}>
                <div className="model-card-heading">
                  <h2>{modelLabels[config.config_kind]}</h2>
                  <button
                    type="button"
                    className="secondary"
                    disabled={testingModel === config.config_kind}
                    onClick={() => void testModel(config)}
                  >
                    Test
                  </button>
                </div>
                {modelTestMessages[config.config_kind] ? (
                  <p
                    className={
                      modelTestMessages[config.config_kind] === "model test succeeded"
                        ? "copy-line"
                        : "notice"
                    }
                  >
                    {modelTestMessages[config.config_kind]}
                  </p>
                ) : null}
                <div className="form">
                <label>
                  Provider
                  <input
                    value={config.provider_name}
                    onChange={(event) =>
                      updateModel(config.config_kind, { provider_name: event.target.value })
                    }
                  />
                </label>
                <label>
                  Base URL
                  <input
                    value={config.provider_base_url}
                    onChange={(event) =>
                      updateModel(config.config_kind, {
                        provider_base_url: event.target.value,
                      })
                    }
                  />
                </label>
                <label>
                  API key
                  <input
                    type="password"
                    value={config.provider_api_key ?? ""}
                    onChange={(event) =>
                      updateModel(config.config_kind, { provider_api_key: event.target.value })
                    }
                  />
                </label>
                <label>
                  Model
                  <input
                    value={config.default_model}
                    onChange={(event) =>
                      updateModel(config.config_kind, { default_model: event.target.value })
                    }
                  />
                </label>
                <label>
                  Timeout seconds
                  <input
                    type="number"
                    min={1}
                    value={config.request_timeout_seconds}
                    onChange={(event) =>
                      updateModel(config.config_kind, {
                        request_timeout_seconds: Number(event.target.value),
                      })
                    }
                  />
                </label>
                {config.config_kind === "llm" ? (
                  <label className="checkbox-row">
                    <input
                      type="checkbox"
                      checked={config.allow_streaming}
                      onChange={(event) =>
                        updateModel(config.config_kind, {
                          allow_streaming: event.target.checked,
                        })
                      }
                    />
                    Streaming
                  </label>
                ) : null}
                </div>
              </section>
            ))}
          </div>
        </form>
      </section>
    );
  }

  if (section === "hermes") {
    return (
      <section className="admin-page" id="admin-hermes">
        <div className="panel-heading">
          <h1>Hermes 管理</h1>
          <button type="button" className="secondary" onClick={() => void refresh()}>
            Refresh
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        {!requiredModelsReady ? <p className="notice">{modelGateMessage}</p> : null}
        <div className="panel">
          <table>
            <thead>
              <tr>
                <th>Owner</th>
                <th>Kind</th>
                <th>Status</th>
                <th>Base URL</th>
                <th>Action</th>
              </tr>
            </thead>
            <tbody>
              {users.map((owner) => {
                const instance = instancesByUserId.get(owner.id);
                return (
                  <tr key={owner.id}>
                    <td>{owner.email}</td>
                    <td>{instance?.kind ?? "not_created"}</td>
                    <td>{instance?.status ?? "not_created"}</td>
                    <td>{instance?.base_url ?? "-"}</td>
                    <td>
                      {!instance ? (
                        <button
                          type="button"
                          className="secondary"
                          disabled={!requiredModelsReady}
                          onClick={() => void createManagedHermes(owner.id)}
                        >
                          Create
                        </button>
                      ) : instance.kind === "managed_docker" ? (
                        <div className="button-row">
                          <button
                            type="button"
                            className="secondary"
                            disabled={!requiredModelsReady}
                            onClick={() => void controlInstance("start", instance)}
                          >
                            Start
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            onClick={() => void controlInstance("stop", instance)}
                          >
                            Stop
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            disabled={!requiredModelsReady}
                            onClick={() => void controlInstance("rebuild", instance)}
                          >
                            Rebuild
                          </button>
                        </div>
                      ) : null}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>
    );
  }

  return (
    <section className="admin-page" id="admin-users">
      <div className="panel-heading">
        <h1>用户管理</h1>
        <button type="button" className="secondary" onClick={() => void refresh()}>
          Refresh
        </button>
      </div>
      {error ? <p className="error">{error}</p> : null}
      <div className="grid-section">
        <div className="panel">
          <h2>Users</h2>
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
          {!requiredModelsReady ? <p className="notice">{modelGateMessage}</p> : null}
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
            <button type="submit" disabled={!requiredModelsReady}>
              Create invite
            </button>
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
      </div>
    </section>
  );
}
