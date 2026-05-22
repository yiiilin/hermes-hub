import type {
  ApiClient,
  HermesInstance,
  Invite,
  ModelApiType,
  ModelConfig,
  ModelConfigKind,
  ReasoningEffort,
  User,
} from "../api/client";
import { useI18n } from "../i18n";
import { FormEvent, useEffect, useMemo, useState } from "react";

type AdminSection = "users" | "models" | "hermes";

type AdminRouteProps = {
  apiClient: ApiClient;
  currentUser: User;
  section: AdminSection;
};

const defaultInviteHours = 24;
const apiTypeLabels: Record<ModelApiType, string> = {
  chat_completions: "Chat Completions",
  responses: "Responses",
  images_generations: "Images",
};
const reasoningEfforts: Array<ReasoningEffort | ""> = ["", "minimal", "low", "medium", "high"];

export function AdminRoute({ apiClient, currentUser, section }: AdminRouteProps) {
  const { language, t } = useI18n();
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
  const [editingExternalUserId, setEditingExternalUserId] = useState<string | null>(null);
  const [externalDraft, setExternalDraft] = useState({
    name: "",
    base_url: "",
    api_token: "",
  });
  const [error, setError] = useState<string | null>(null);

  const instancesByUserId = useMemo(
    () => new Map(instances.map((instance) => [instance.user_id, instance])),
    [instances],
  );
  const modelLabels: Record<ModelConfigKind, string> = {
    llm: t("admin.llm"),
    image: t("admin.imageModel"),
    title: t("admin.titleModel"),
  };

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
      setError(cause instanceof Error ? cause.message : t("chat.workspaceLoadFailed"));
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
      [config.config_kind]: t("admin.modelTesting"),
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
          cause instanceof Error ? cause.message : t("admin.modelTestFailed"),
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

  function openExternalEditor(instance: HermesInstance) {
    setEditingExternalUserId(instance.user_id);
    setExternalDraft({
      name: instance.name ?? "",
      base_url: instance.base_url,
      api_token: "",
    });
  }

  async function saveExternalHermesConfig(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!editingExternalUserId) {
      return;
    }
    await apiClient.updateExternalHermesInstanceConfig(editingExternalUserId, externalDraft);
    setEditingExternalUserId(null);
    setExternalDraft({ name: "", base_url: "", api_token: "" });
    await refresh();
  }

  const missingRequiredModelNames =
    missingRequiredModels.length > 0
      ? missingRequiredModels.map((kind) => modelLabels[kind]).join(language === "zh" ? "、" : ", ")
      : [modelLabels.llm, modelLabels.title].join(language === "zh" ? "、" : ", ");
  const modelGateMessage = t("admin.modelGate", { models: missingRequiredModelNames });

  if (section === "models") {
    return (
      <section className="admin-page" id="admin-models">
        <form className="admin-page" onSubmit={(event) => void saveModels(event)}>
          <div className="panel-heading">
            <h1>{t("admin.modelConfig")}</h1>
            <div className="button-row">
              <button type="button" className="secondary" onClick={() => void refresh()}>
                {t("admin.refresh")}
              </button>
              <button type="submit">{t("admin.save")}</button>
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
                    {t("admin.test")}
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
                    {t("admin.provider")}
                    <input
                      value={config.provider_name}
                      onChange={(event) =>
                        updateModel(config.config_kind, { provider_name: event.target.value })
                      }
                    />
                  </label>
                  <label>
                    {t("admin.baseUrl")}
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
                    {t("admin.apiKey")}
                    <input
                      type="password"
                      value={config.provider_api_key ?? ""}
                      onChange={(event) =>
                        updateModel(config.config_kind, { provider_api_key: event.target.value })
                      }
                    />
                  </label>
                  <label>
                    {t("admin.model")}
                    <input
                      value={config.default_model}
                      onChange={(event) =>
                        updateModel(config.config_kind, { default_model: event.target.value })
                      }
                    />
                  </label>
                  <label>
                    {t("admin.api")}
                    <select
                      value={config.api_type}
                      disabled={config.config_kind === "image"}
                      onChange={(event) =>
                        updateModel(config.config_kind, {
                          api_type: event.target.value as ModelApiType,
                        })
                      }
                    >
                      {(config.config_kind === "image"
                        ? ["images_generations"]
                        : ["chat_completions", "responses"]
                      ).map((apiType) => (
                        <option key={apiType} value={apiType}>
                          {apiTypeLabels[apiType as ModelApiType]}
                        </option>
                      ))}
                    </select>
                  </label>
                  {config.config_kind !== "image" ? (
                    <label>
                      {t("admin.reasoning")}
                      <select
                        value={config.reasoning_effort ?? ""}
                        onChange={(event) =>
                          updateModel(config.config_kind, {
                            reasoning_effort:
                              event.target.value === ""
                                ? null
                                : (event.target.value as ReasoningEffort),
                          })
                        }
                      >
                        {reasoningEfforts.map((effort) => (
                          <option key={effort || "none"} value={effort}>
                            {effort || t("admin.noReasoning")}
                          </option>
                        ))}
                      </select>
                    </label>
                  ) : null}
                  <label>
                    {t("admin.timeout")}
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
                      {t("admin.streaming")}
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
          <h1>{t("admin.title")}</h1>
          <button type="button" className="secondary" onClick={() => void refresh()}>
            {t("admin.refresh")}
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        {!requiredModelsReady ? <p className="notice">{modelGateMessage}</p> : null}
        <div className="panel">
          <table>
            <thead>
              <tr>
                <th>{t("admin.owner")}</th>
                <th>{t("admin.kind")}</th>
                <th>{t("admin.status")}</th>
                <th>{t("admin.baseUrl")}</th>
                <th>{t("admin.action")}</th>
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
                          {t("admin.create")}
                        </button>
                      ) : instance.kind === "managed_docker" ? (
                        <div className="button-row">
                          <button
                            type="button"
                            className="secondary"
                            disabled={!requiredModelsReady}
                            onClick={() => void controlInstance("start", instance)}
                          >
                            {t("admin.start")}
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            onClick={() => void controlInstance("stop", instance)}
                          >
                            {t("admin.stop")}
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            disabled={!requiredModelsReady}
                            onClick={() => void controlInstance("rebuild", instance)}
                          >
                            {t("admin.rebuild")}
                          </button>
                        </div>
                      ) : (
                        <button
                          type="button"
                          className="secondary"
                          onClick={() => openExternalEditor(instance)}
                        >
                          {t("admin.edit")}
                        </button>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
        {editingExternalUserId ? (
          <form className="panel form" onSubmit={(event) => void saveExternalHermesConfig(event)}>
            <div className="panel-heading">
              <h2>{t("admin.hermesConfig")}</h2>
              <button
                type="button"
                className="secondary"
                onClick={() => setEditingExternalUserId(null)}
              >
                {t("admin.cancel")}
              </button>
            </div>
            <label>
              {t("admin.name")}
              <input
                value={externalDraft.name}
                onChange={(event) =>
                  setExternalDraft((draft) => ({ ...draft, name: event.target.value }))
                }
                required
              />
            </label>
            <label>
              {t("admin.baseUrl")}
              <input
                value={externalDraft.base_url}
                onChange={(event) =>
                  setExternalDraft((draft) => ({ ...draft, base_url: event.target.value }))
                }
                required
              />
            </label>
            <label>
              {t("admin.apiKey")}
              <input
                type="password"
                value={externalDraft.api_token}
                onChange={(event) =>
                  setExternalDraft((draft) => ({ ...draft, api_token: event.target.value }))
                }
                placeholder={t("admin.externalTokenHint")}
              />
            </label>
            <div className="button-row">
              <button type="submit">{t("admin.saveConfig")}</button>
            </div>
          </form>
        ) : null}
      </section>
    );
  }

  return (
    <section className="admin-page" id="admin-users">
      <div className="panel-heading">
        <h1>{t("admin.userManagement")}</h1>
        <button type="button" className="secondary" onClick={() => void refresh()}>
          {t("admin.refresh")}
        </button>
      </div>
      {error ? <p className="error">{error}</p> : null}
      <div className="grid-section">
        <div className="panel">
          <h2>{t("admin.users")}</h2>
          <table>
            <thead>
              <tr>
                <th>{t("admin.email")}</th>
                <th>{t("admin.role")}</th>
                <th>{t("admin.status")}</th>
                <th>{t("admin.action")}</th>
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
                      {user.status === "active" ? t("admin.disable") : t("admin.enable")}
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>

        <div className="panel">
          <h2>{t("admin.invites")}</h2>
          {!requiredModelsReady ? <p className="notice">{modelGateMessage}</p> : null}
          <form className="inline-form" onSubmit={createInvite}>
            <label>
              {t("admin.hours")}
              <input
                type="number"
                min={1}
                value={inviteHours}
                onChange={(event) => setInviteHours(Number(event.target.value))}
                required
              />
            </label>
            <label>
              {t("admin.uses")}
              <input
                type="number"
                min={1}
                value={inviteMaxUses}
                onChange={(event) => setInviteMaxUses(Number(event.target.value))}
                required
              />
            </label>
            <button type="submit" disabled={!requiredModelsReady}>
              {t("admin.createInvite")}
            </button>
          </form>
          {lastInviteLink ? <p className="copy-line">{lastInviteLink}</p> : null}
          <ul className="list compact-list">
            {invites.map((invite) => (
              <li key={invite.id}>
                <strong>{invite.status}</strong>
                <span>
                  {invite.used_count}/{invite.max_uses} {t("admin.used")} ·{" "}
                  {t("admin.expiresAt")}{" "}
                  {new Date(invite.expires_at * 1000).toLocaleString(language)}
                </span>
                {invite.status === "pending" ? (
                  <button
                    type="button"
                    className="secondary"
                    onClick={() => void apiClient.revokeInvite(invite.id).then(refresh)}
                  >
                    {t("admin.revoke")}
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
