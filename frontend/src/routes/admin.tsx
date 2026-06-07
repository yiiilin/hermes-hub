import type {
  CreateIntegrationAppInput,
  ApiClient,
  HermesProfile,
  HermesInstance,
  HermesScheduledTaskSnapshot,
  HermesSchedulerSnapshot,
  Invite,
  ManagedSkill,
  ManagedSkillTreeNode,
  IntegrationApp,
  IntegrationToolDefinition,
  ModelApiType,
  ModelConfig,
  ModelFallbackConfig,
  ModelConfigKind,
  PublicPlatformHermesStatus,
  PublicPlatformSessionPage,
  PublicPlatformSessionSummary,
  ReasoningEffort,
  SpeechInputConfig,
  SystemSettings,
  UpdateIntegrationAppInput,
  User,
} from "../api/client";
import {
  defaultApiManagementSettings,
  defaultLdapSettings,
  defaultOidcSettings,
  defaultPublicPlatformSettings,
  defaultSpeechInputSettings,
} from "../api/client";
import { useI18n } from "../i18n";
import type { AdminSettingsTab } from "../navigation";
import { ChangeEvent, FormEvent, ReactNode, useEffect, useMemo, useRef, useState } from "react";
import {
  ExternalLink,
  FilePlus2,
  FileText,
  Folder,
  FolderPlus,
  KeyRound,
  Plus,
  RotateCcw,
  Upload,
  X,
} from "lucide-react";
import Vditor from "vditor";
import "vditor/dist/index.css";

type AdminRouteProps = {
  activeTab: AdminSettingsTab;
  apiClient: ApiClient;
  currentUser: User;
  onTabChange: (tab: AdminSettingsTab) => void;
};

type SelectedSkillNode = {
  path: string;
  kind: "dir" | "file";
} | null;

type HermesAction = "create" | "start" | "stop" | "rebuild";
type ModelTestTarget = "primary" | "fallback";

type HermesSchedulerTaskRow = {
  snapshot: HermesSchedulerSnapshot;
  task: HermesScheduledTaskSnapshot;
};

const defaultInviteHours = 24;
const bytesPerMegabyte = 1024 * 1024;
const hermesIdleStopAfterSeconds = 30 * 60;
const publicSessionsPageSize = 10;
const modelConfigOrder: ModelConfigKind[] = ["llm", "title", "image"];
const integrationAppCallbackUrlPattern =
  /^https?:\/\/([^/?#:@\s]+|\[[0-9A-Fa-f:.]+\])(?::(?:6553[0-5]|655[0-2][0-9]|65[0-4][0-9]{2}|6[0-4][0-9]{3}|[1-5][0-9]{4}|[1-9][0-9]{0,3}|0))?(?:[/?#][^\s]*)?$/i;
const apiTypeLabels: Record<ModelApiType, string> = {
  chat_completions: "Chat Completions",
  responses: "Responses",
  images_generations: "Images",
};
const reasoningEfforts: Array<ReasoningEffort | ""> = ["", "minimal", "low", "medium", "high"];

type IntegrationAppForm = {
  name: string;
  enabled: boolean;
  redirect_uri: string;
  scopes: string;
  authorization_code_ttl_seconds: number;
  hidden_session_idle_timeout_seconds: number;
  default_tool_timeout_seconds: number;
  max_tool_timeout_seconds: number;
};

function blankIntegrationAppForm(): IntegrationAppForm {
  return {
    name: "",
    enabled: true,
    redirect_uri: "",
    scopes: "openid profile email",
    authorization_code_ttl_seconds: 600,
    hidden_session_idle_timeout_seconds: 3600,
    default_tool_timeout_seconds: 60,
    max_tool_timeout_seconds: 300,
  };
}

function integrationAppFormFromApp(app: IntegrationApp): IntegrationAppForm {
  return {
    name: app.name,
    enabled: app.enabled,
    redirect_uri: app.redirect_uri,
    scopes: app.scopes,
    authorization_code_ttl_seconds: app.authorization_code_ttl_seconds,
    hidden_session_idle_timeout_seconds: app.hidden_session_idle_timeout_seconds,
    default_tool_timeout_seconds: app.default_tool_timeout_seconds,
    max_tool_timeout_seconds: app.max_tool_timeout_seconds,
  };
}

function isValidIntegrationAppRedirectUri(value: string): boolean {
  return integrationAppCallbackUrlPattern.test(value.trim());
}

function isValidPositiveInteger(value: number): boolean {
  return Number.isInteger(value) && value > 0;
}

function formatIntegrationToolParameters(parameters: unknown): string {
  return JSON.stringify(parameters ?? {}, null, 2);
}

function latestIntegrationToolSyncAt(tools: IntegrationToolDefinition[]): number | null {
  if (tools.length === 0) {
    return null;
  }
  return tools.reduce((latest, tool) => Math.max(latest, tool.updated_at), 0);
}

function modelTestKey(kind: ModelConfigKind, target: ModelTestTarget): string {
  return `${kind}:${target}`;
}

function fallbackConfigForModel(config: ModelConfig): ModelFallbackConfig {
  return (
    config.fallback ?? {
      enabled: false,
      provider_name: config.provider_name,
      provider_base_url: config.provider_base_url,
      provider_api_key: config.provider_api_key ?? "",
      default_model: config.default_model,
      allowed_models: config.default_model ? [config.default_model] : [],
      api_type: config.config_kind === "image" ? "images_generations" : config.api_type,
      reasoning_effort: config.reasoning_effort ?? null,
      allow_streaming: config.config_kind === "llm" ? config.allow_streaming : false,
      request_timeout_seconds: config.request_timeout_seconds || 60,
      context_window_tokens: config.context_window_tokens || 128000,
      max_output_tokens: config.max_output_tokens || 4096,
      temperature: Number.isFinite(config.temperature) ? config.temperature : 0.7,
      supports_parallel_tools:
        config.config_kind === "llm" ? config.supports_parallel_tools !== false : false,
    }
  );
}

function multilineListToString(values: string[]): string {
  return values.join("\n");
}

function multilineStringToList(value: string): string[] {
  return value
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter((line) => line.length > 0);
}

function MarkdownVditorEditor({
  value,
  label,
  onChange,
  className,
  height = 520,
}: {
  className?: string;
  height?: number;
  value: string;
  label: string;
  onChange: (value: string) => void;
}) {
  const { language } = useI18n();
  const editorHostRef = useRef<HTMLDivElement | null>(null);
  const editorInstanceRef = useRef<Vditor | null>(null);
  const latestOnChangeRef = useRef(onChange);
  const latestValueRef = useRef(value);
  const lastEmittedMarkdownRef = useRef<string | null>(null);
  const editorReadyRef = useRef(false);

  useEffect(() => {
    latestOnChangeRef.current = onChange;
  }, [onChange]);

  useEffect(() => {
    latestValueRef.current = value;
    const editor = editorInstanceRef.current;
    if (!editor) {
      return;
    }
    if (!editorReadyRef.current || value === lastEmittedMarkdownRef.current) {
      return;
    }
    if (editor.getValue() !== value) {
      editor.setValue(value, true);
    }
  }, [value]);

  useEffect(() => {
    const editorHost = editorHostRef.current;
    if (!editorHost) {
      return;
    }
    let destroyed = false;
    const emitMarkdown = (nextValue: string) => {
      lastEmittedMarkdownRef.current = nextValue;
      latestValueRef.current = nextValue;
      latestOnChangeRef.current(nextValue);
    };
    const editor = new Vditor(editorHost, {
      cache: { enable: false },
      cdn: "/vditor",
      height,
      lang: language === "zh" ? "zh_CN" : "en_US",
      minHeight: height,
      mode: "wysiwyg",
      value,
      toolbar: [
        "headings",
        "bold",
        "italic",
        "strike",
        "|",
        "list",
        "ordered-list",
        "check",
        "|",
        "quote",
        "code",
        "inline-code",
        "table",
        "link",
        "|",
        "undo",
        "redo",
        "fullscreen",
        "edit-mode",
      ],
      toolbarConfig: { pin: false },
      input: emitMarkdown,
      blur: emitMarkdown,
      after: () => {
        if (destroyed) {
          return;
        }
        editorReadyRef.current = true;
        const latestValue = latestValueRef.current;
        // Vditor 初始化依赖异步 Lute 资源；初始化期间如果后端刷新了内容，这里补一次同步。
        if (editor.getValue() !== latestValue) {
          editor.setValue(latestValue, true);
        }
      },
    });
    editorInstanceRef.current = editor;

    return () => {
      destroyed = true;
      editorReadyRef.current = false;
      editorInstanceRef.current = null;
      editor.destroy();
    };
  }, [height, language]);

  return (
    <div className="soul-markdown-editor">
      <div className="markdown-editor-label">{label}</div>
      <div
        ref={editorHostRef}
        className={`markdown-vditor-editor ${className ?? ""}`.trim()}
        role="textbox"
        aria-label={label}
      />
    </div>
  );
}

function megabytesFromBytes(value: number): number {
  return Math.max(1, Math.round(value / bytesPerMegabyte));
}

function bytesFromMegabytes(value: number): number {
  return Math.max(1, Math.round(value)) * bytesPerMegabyte;
}

function formatBytes(size: number): string {
  if (size < 1024) {
    return `${size} B`;
  }
  if (size < 1024 * 1024) {
    return `${(size / 1024).toFixed(1)} KB`;
  }
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

type HermesInstanceStatusDisplay = {
  label: string;
  detail?: string;
};

function hermesInstanceStatusDisplay(instance?: HermesInstance): HermesInstanceStatusDisplay {
  if (!instance) {
    return { label: "not_created" };
  }
  if (instance.status === "error") {
    return {
      label: "error",
      detail: readableHermesStatusDetail(instance),
    };
  }
  const healthStatus = instance.health_status?.trim();
  if (healthStatus && !["unknown", "running"].includes(healthStatus)) {
    return { label: healthStatus };
  }
  return { label: instance.status };
}

function readableHermesStatusDetail(instance: HermesInstance): string | undefined {
  const statusMessage = instance.status_message?.trim();
  if (statusMessage) {
    return statusMessage;
  }
  const healthStatus = instance.health_status?.trim();
  if (healthStatus && !["unknown", "error"].includes(healthStatus)) {
    return healthStatus;
  }
  return undefined;
}

function formatHermesRuntimeVersion(instance?: HermesInstance): string {
  const reportedVersion = instance?.runtime_version?.trim();
  if (reportedVersion && reportedVersion !== "latest") {
    return reportedVersion;
  }
  return hermesImageVersion(instance?.runtime_image) ?? "-";
}

function hermesImageVersion(image?: string | null): string | undefined {
  const imageWithoutDigest = image?.split("@")[0]?.trim();
  const lastSegment = imageWithoutDigest?.split("/").at(-1);
  const tag = lastSegment?.includes(":") ? lastSegment.split(":").at(-1)?.trim() : undefined;
  return tag && tag !== "latest" ? tag : undefined;
}

function formatSchedulerSnapshotTime(
  value: number | string | null | undefined,
  language: string,
): string {
  if (value === null || value === undefined || value === "") {
    return "-";
  }
  // Hermes adapter 可能上送秒级 Unix 时间，也可能上送 ISO 字符串；展示层统一容错格式化。
  const timestamp = typeof value === "number" && value < 10_000_000_000 ? value * 1000 : value;
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return String(value);
  }
  return new Intl.DateTimeFormat(language === "zh" ? "zh-CN" : "en-US", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function formatHermesStartedAt(instance: HermesInstance | undefined, language: string): string {
  return formatSchedulerSnapshotTime(instance?.last_started_at, language);
}

function formatHermesStopTime(
  instance: HermesInstance | undefined,
  language: string,
  t: (key: "admin.estimatedStopAt", values?: Record<string, string | number>) => string,
): string {
  if (!instance) {
    return "-";
  }
  if (instance.status === "stopped") {
    return formatSchedulerSnapshotTime(instance.last_stopped_at, language);
  }
  const lastActivity = instance.last_user_activity_at ?? instance.last_started_at;
  if (lastActivity === null || lastActivity === undefined) {
    return "-";
  }
  const estimatedStopAt =
    typeof lastActivity === "number" ? lastActivity + hermesIdleStopAfterSeconds : lastActivity;
  return t("admin.estimatedStopAt", {
    time: formatSchedulerSnapshotTime(estimatedStopAt, language),
  });
}

function publicPlatformSessionLink(session: PublicPlatformSessionSummary): string {
  const publicPath =
    session.public_url?.trim() || `/public/sessions/${encodeURIComponent(session.id)}`;
  if (/^https?:\/\//i.test(publicPath)) {
    return publicPath;
  }
  return `${window.location.origin}${publicPath.startsWith("/") ? publicPath : `/${publicPath}`}`;
}

function parentPath(path: string): string {
  return path.split("/").slice(0, -1).join("/");
}

function defaultFilePathForDirectory(path: string): string {
  return path ? `${path}/SKILL.md` : "writing/SKILL.md";
}

function defaultChildDirectoryPath(path: string): string {
  return path ? `${path}/new-folder` : "new-folder";
}

function uploadedFileName(file: File): string {
  return (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name;
}

function hasHiddenManagedSkillSegment(path: string): boolean {
  return path
    .split("/")
    .filter(Boolean)
    .some((segment) => segment.startsWith("."));
}

function findManagedSkillTreeNode(
  node: ManagedSkillTreeNode,
  path: string,
): ManagedSkillTreeNode | null {
  if (node.path === path) {
    return node;
  }
  for (const child of node.children) {
    const found = findManagedSkillTreeNode(child, path);
    if (found) {
      return found;
    }
  }
  return null;
}

function collectManagedSkillDirectories(node: ManagedSkillTreeNode): string[] {
  const directories = node.kind === "dir" && node.path ? [node.path] : [];
  for (const child of node.children) {
    directories.push(...collectManagedSkillDirectories(child));
  }
  return directories;
}

function managedSkillTreeFromList(skills: ManagedSkill[]): ManagedSkillTreeNode {
  const root: ManagedSkillTreeNode = {
    name: "",
    path: "",
    kind: "dir",
    size: 0,
    children: [],
  };

  function ensureDir(path: string) {
    let node = root;
    let currentPath = "";
    for (const segment of path.split("/").filter(Boolean)) {
      currentPath = currentPath ? `${currentPath}/${segment}` : segment;
      let child = node.children.find((item) => item.kind === "dir" && item.name === segment);
      if (!child) {
        child = {
          name: segment,
          path: currentPath,
          kind: "dir",
          size: 0,
          children: [],
        };
        node.children.push(child);
      }
      node = child;
    }
    return node;
  }

  for (const skill of skills) {
    if (hasHiddenManagedSkillSegment(skill.path)) {
      continue;
    }
    const segments = skill.path.split("/").filter(Boolean);
    const name = segments.pop();
    if (!name) {
      continue;
    }
    ensureDir(segments.join("/")).children.push({
      name,
      path: skill.path,
      kind: "file",
      size: skill.size,
      children: [],
    });
  }

  // 旧后端还没有 tree 接口时只能从文件列表推导目录；排序保持和后端树接口一致。
  function sortNode(node: ManagedSkillTreeNode) {
    node.children.sort((left, right) => {
      if (left.kind !== right.kind) {
        return left.kind === "dir" ? -1 : 1;
      }
      return left.name.localeCompare(right.name);
    });
    for (const child of node.children) {
      sortNode(child);
    }
  }
  sortNode(root);
  return root;
}

export function AdminRoute({ activeTab, apiClient, currentUser, onTabChange }: AdminRouteProps) {
  const { language, t } = useI18n();
  const [users, setUsers] = useState<User[]>([]);
  const [invites, setInvites] = useState<Invite[]>([]);
  const [instances, setInstances] = useState<HermesInstance[]>([]);
  const [schedulerSnapshots, setSchedulerSnapshots] = useState<HermesSchedulerSnapshot[]>([]);
  const [hermesProfile, setHermesProfile] = useState<HermesProfile>({
    soul_md: "",
  });
  const [hermesProfileSaved, setHermesProfileSaved] = useState(false);
  const [modelConfigs, setModelConfigs] = useState<ModelConfig[]>([]);
  const [managedSkills, setManagedSkills] = useState<ManagedSkill[]>([]);
  const [managedSkillTree, setManagedSkillTree] = useState<ManagedSkillTreeNode | null>(null);
  const [selectedSkillNode, setSelectedSkillNode] = useState<SelectedSkillNode>(null);
  const [skillPathInput, setSkillPathInput] = useState("");
  const [skillContent, setSkillContent] = useState("");
  const [skillSaved, setSkillSaved] = useState(false);
  const [skillLoading, setSkillLoading] = useState(false);
  const [skillEditorMode, setSkillEditorMode] = useState<"file" | "directory">("file");
  const fileUploadInputRef = useRef<HTMLInputElement | null>(null);
  const folderUploadInputRef = useRef<HTMLInputElement | null>(null);
  const [systemSettings, setSystemSettings] = useState<SystemSettings>({
    max_sessions_per_user: 20,
    max_attachment_upload_bytes: 200 * 1024 * 1024,
    attachment_retention_days: 7,
    empty_chat_prompt: "",
    speech_input: defaultSpeechInputSettings(),
    public_platform: defaultPublicPlatformSettings(),
    api_management: defaultApiManagementSettings(),
    oidc: defaultOidcSettings(),
    ldap: defaultLdapSettings(),
  });
  const [speechInputRuntimeConfig, setSpeechInputRuntimeConfig] = useState<SpeechInputConfig>({
    enabled: false,
    runtime_available: false,
    max_duration_seconds: 60,
    sample_rate: 16000,
    model: "",
  });
  const [publicPlatformHermesStatus, setPublicPlatformHermesStatus] =
    useState<PublicPlatformHermesStatus>({
      enabled: false,
      ready: false,
      hermes_instance: null,
    });
  const [publicSessionsPage, setPublicSessionsPage] = useState<PublicPlatformSessionPage>({
    sessions: [],
    page: 1,
    page_size: publicSessionsPageSize,
    total: 0,
    total_pages: 0,
  });
  const [publicSessionsLoading, setPublicSessionsLoading] = useState(false);
  const [forceClearingPublicSessionId, setForceClearingPublicSessionId] = useState<string | null>(
    null,
  );
  const [settingsSaved, setSettingsSaved] = useState(false);
  const [systemSettingsLoaded, setSystemSettingsLoaded] = useState(false);
  const [inviteHours, setInviteHours] = useState(defaultInviteHours);
  const [inviteMaxUses, setInviteMaxUses] = useState(1);
  const [lastInviteLink, setLastInviteLink] = useState<string | null>(null);
  const [requiredModelsReady, setRequiredModelsReady] = useState(false);
  const [missingRequiredModels, setMissingRequiredModels] = useState<ModelConfigKind[]>([]);
  const [modelTestMessages, setModelTestMessages] = useState<Record<string, string>>({});
  const [modelSaved, setModelSaved] = useState(false);
  const [testingModel, setTestingModel] = useState<string | null>(null);
  const [pendingHermesAction, setPendingHermesAction] = useState<{
    userId: string;
    action: HermesAction;
  } | null>(null);
  const [publicHermesRebuilding, setPublicHermesRebuilding] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [apiManagementEnabledForTabs, setApiManagementEnabledForTabs] = useState(false);
  const [integrationApps, setIntegrationApps] = useState<IntegrationApp[]>([]);
  const [integrationAppDialogOpen, setIntegrationAppDialogOpen] = useState(false);
  const [selectedIntegrationAppId, setSelectedIntegrationAppId] = useState<string | null>(null);
  const [integrationAppForm, setIntegrationAppForm] = useState<IntegrationAppForm>(
    blankIntegrationAppForm(),
  );
  const [integrationAppSecret, setIntegrationAppSecret] = useState<string | null>(null);
  const [integrationAppSaved, setIntegrationAppSaved] = useState(false);
  const [integrationTools, setIntegrationTools] = useState<IntegrationToolDefinition[]>([]);
  const [integrationToolsLoading, setIntegrationToolsLoading] = useState(false);
  const integrationAppNameInputRef = useRef<HTMLInputElement | null>(null);
  const selectedIntegrationAppIdRef = useRef<string | null>(null);

  const instancesByUserId = useMemo(
    () => new Map(instances.map((instance) => [instance.user_id, instance])),
    [instances],
  );
  const usersById = useMemo(() => new Map(users.map((user) => [user.id, user])), [users]);
  const selectedIntegrationApp = useMemo(
    () => integrationApps.find((app) => app.id === selectedIntegrationAppId) ?? null,
    [integrationApps, selectedIntegrationAppId],
  );
  const integrationToolsLastSyncedAt = useMemo(
    () => latestIntegrationToolSyncAt(integrationTools),
    [integrationTools],
  );
  useEffect(() => {
    selectedIntegrationAppIdRef.current = selectedIntegrationAppId;
  }, [selectedIntegrationAppId]);
  useEffect(() => {
    if (!integrationAppDialogOpen) {
      return;
    }
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        closeIntegrationAppDialog();
      }
    }
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [integrationAppDialogOpen]);
  useEffect(() => {
    if (!integrationAppDialogOpen) {
      return;
    }
    integrationAppNameInputRef.current?.focus();
  }, [integrationAppDialogOpen, selectedIntegrationAppId]);
  const schedulerTaskRows = useMemo<HermesSchedulerTaskRow[]>(
    () =>
      schedulerSnapshots.flatMap((snapshot) =>
        (snapshot.tasks ?? []).map((task) => ({ snapshot, task })),
      ),
    [schedulerSnapshots],
  );
  const modelLabels: Record<ModelConfigKind, string> = {
    llm: t("admin.llm"),
    image: t("admin.imageModel"),
    title: t("admin.titleModel"),
  };
  const orderedModelConfigs = useMemo(
    () =>
      [...modelConfigs].sort(
        (left, right) =>
          modelConfigOrder.indexOf(left.config_kind) - modelConfigOrder.indexOf(right.config_kind),
      ),
    [modelConfigs],
  );
  const oidcRedirectUri = useMemo(() => `${window.location.origin}/api/auth/oidc/callback`, []);
  const apiDocsUrl = useMemo(() => `${window.location.origin}/api/docs`, []);
  const apiOpenApiUrl = useMemo(() => `${window.location.origin}/api/docs/openapi.json`, []);
  const integrationAppToolsSyncUrl = useMemo(
    () => `${window.location.origin}/api/integrations/apps/self/tools`,
    [],
  );
  const adminSettingsTabs: Array<{ key: AdminSettingsTab; label: string }> = [
    { key: "users", label: t("admin.userManagement") },
    { key: "models", label: t("admin.modelConfig") },
    { key: "hermes", label: t("admin.title") },
    { key: "profile", label: t("admin.hermesProfile") },
    { key: "scheduler", label: t("admin.scheduledTasks") },
    { key: "skills", label: t("admin.skillManagement") },
    { key: "system", label: t("admin.systemParameters") },
    { key: "auth", label: t("admin.authSettings") },
    { key: "api-management", label: t("admin.apiManagementSettings") },
    ...(apiManagementEnabledForTabs
      ? [{ key: "integration-apps" as const, label: t("admin.integrationApps") }]
      : []),
    { key: "public-platform", label: t("admin.publicPlatform") },
  ];

  async function fetchPublicPlatformSessionsPage(page: number) {
    const nextPage = await apiClient.listPublicPlatformSessions({
      page,
      pageSize: publicSessionsPageSize,
    });
    if (nextPage.sessions.length === 0 && nextPage.total > 0 && nextPage.page > 1) {
      return apiClient.listPublicPlatformSessions({
        page: Math.max(nextPage.total_pages, 1),
        pageSize: publicSessionsPageSize,
      });
    }
    return nextPage;
  }

  async function refresh() {
    setError(null);
    try {
      if (
        activeTab === "auth" ||
        activeTab === "system" ||
        activeTab === "api-management" ||
        activeTab === "integration-apps" ||
        activeTab === "public-platform"
      ) {
        setSystemSettingsLoaded(false);
      }
      const [
        nextUsers,
        nextInvites,
        nextInstances,
        nextModelStatus,
        nextSettings,
        nextSchedulerSnapshots,
        nextHermesProfile,
        nextSpeechInputRuntimeConfig,
        nextPublicPlatformHermesStatus,
        nextPublicSessionsPage,
      ] = await Promise.all([
        apiClient.listUsers(),
        apiClient.listInvites(),
        apiClient.listHermesInstances(),
        apiClient.modelConfigStatus(),
        apiClient.systemSettings(),
        activeTab === "scheduler"
          ? apiClient.listHermesSchedulerSnapshots()
          : Promise.resolve(null),
        activeTab === "profile" ? apiClient.hermesProfile() : Promise.resolve(null),
        activeTab === "system" ? apiClient.speechInputConfig() : Promise.resolve(null),
        activeTab === "public-platform"
          ? apiClient.publicPlatformHermesInstance()
          : Promise.resolve(null),
        activeTab === "public-platform"
          ? fetchPublicPlatformSessionsPage(publicSessionsPage.page)
          : Promise.resolve(null),
      ]);
      const nextIntegrationApps =
        activeTab === "integration-apps" && nextSettings.api_management.enabled
          ? await apiClient.listIntegrationApps()
          : null;
      setUsers(nextUsers);
      setInvites(nextInvites);
      setInstances(nextInstances);
      setModelConfigs(nextModelStatus.model_configs);
      setRequiredModelsReady(nextModelStatus.required_models_ready);
      setMissingRequiredModels(nextModelStatus.missing_required_model_config_kinds);
      setSystemSettings(nextSettings);
      setApiManagementEnabledForTabs(nextSettings.api_management.enabled);
      setSystemSettingsLoaded(true);
      if (nextIntegrationApps) {
        setIntegrationApps(sortIntegrationApps(nextIntegrationApps));
        if (
          selectedIntegrationAppIdRef.current &&
          !nextIntegrationApps.some((app) => app.id === selectedIntegrationAppIdRef.current)
        ) {
          resetIntegrationAppForm();
        }
      }
      if (nextSchedulerSnapshots) {
        setSchedulerSnapshots(nextSchedulerSnapshots);
      }
      if (nextHermesProfile) {
        setHermesProfile(nextHermesProfile);
      }
      if (nextSpeechInputRuntimeConfig) {
        setSpeechInputRuntimeConfig(nextSpeechInputRuntimeConfig);
      }
      if (nextPublicPlatformHermesStatus) {
        setPublicPlatformHermesStatus(nextPublicPlatformHermesStatus);
      }
      if (nextPublicSessionsPage) {
        setPublicSessionsPage(nextPublicSessionsPage);
      }
      if (activeTab === "skills") {
        await refreshManagedSkills();
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.workspaceLoadFailed"));
    }
  }

  useEffect(() => {
    void refresh();
  }, [activeTab]);

  useEffect(() => {
    if (
      activeTab === "integration-apps" &&
      systemSettingsLoaded &&
      !apiManagementEnabledForTabs
    ) {
      onTabChange("api-management");
    }
  }, [activeTab, apiManagementEnabledForTabs, onTabChange, systemSettingsLoaded]);

  function selectAdminTab(tab: AdminSettingsTab) {
    setError(null);
    setModelSaved(false);
    setSettingsSaved(false);
    setSkillSaved(false);
    setHermesProfileSaved(false);
    if (
      tab === "auth" ||
      tab === "system" ||
      tab === "api-management" ||
      tab === "integration-apps" ||
      tab === "public-platform"
    ) {
      setSystemSettingsLoaded(false);
    }
    onTabChange(tab);
  }

  async function refreshManagedSkills() {
    const nextSkills = await apiClient.listManagedSkills();
    let nextTree: ManagedSkillTreeNode;
    try {
      nextTree = await apiClient.listManagedSkillTree();
    } catch (cause) {
      if (!(cause instanceof Error) || cause.message !== "managed skill not found") {
        throw cause;
      }
      nextTree = managedSkillTreeFromList(nextSkills);
    }
    setManagedSkills(nextSkills);
    setManagedSkillTree(nextTree);
  }

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
    setModelSaved(false);
    setError(null);
    try {
      await apiClient.updateModelConfigs(modelConfigs);
      await refresh();
      // 保存后给管理员一个明确反馈，避免开关变更看起来像“点了没反应”。
      setModelSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.modelSaveFailed"));
    }
  }

  function updateModel(kind: ModelConfigKind, patch: Partial<ModelConfig>) {
    setModelSaved(false);
    setModelConfigs((configs) =>
      configs.map((config) => (config.config_kind === kind ? { ...config, ...patch } : config)),
    );
  }

  function updateModelFallback(kind: ModelConfigKind, patch: Partial<ModelFallbackConfig>) {
    setModelSaved(false);
    setModelConfigs((configs) =>
      configs.map((config) =>
        config.config_kind === kind
          ? {
              ...config,
              fallback: {
                ...fallbackConfigForModel(config),
                ...patch,
              },
            }
          : config,
      ),
    );
  }

  async function testModel(config: ModelConfig, target: ModelTestTarget = "primary") {
    const key = modelTestKey(config.config_kind, target);
    setTestingModel(key);
    setModelTestMessages((messages) => ({
      ...messages,
      [key]: t("admin.modelTesting"),
    }));
    try {
      const result =
        target === "fallback"
          ? await apiClient.testModelFallbackConfig(config)
          : await apiClient.testModelConfig(config);
      setModelTestMessages((messages) => ({
        ...messages,
        [key]: result.ok ? result.message : `HTTP ${result.status_code}: ${result.message}`,
      }));
    } catch (cause) {
      setModelTestMessages((messages) => ({
        ...messages,
        [key]: cause instanceof Error ? cause.message : t("admin.modelTestFailed"),
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

  function isHermesActionPending(userId: string) {
    return pendingHermesAction?.userId === userId;
  }

  function hermesActionLabel(
    userId: string,
    action: HermesAction,
    fallbackKey: "admin.create" | "admin.start" | "admin.stop" | "admin.rebuild",
  ) {
    if (pendingHermesAction?.userId === userId && pendingHermesAction.action === action) {
      const pendingKeys: Record<
        HermesAction,
        "admin.creating" | "admin.starting" | "admin.stopping" | "admin.rebuilding"
      > = {
        create: "admin.creating",
        start: "admin.starting",
        stop: "admin.stopping",
        rebuild: "admin.rebuilding",
      };
      return t(pendingKeys[action]);
    }
    return t(fallbackKey);
  }

  async function controlInstance(action: "start" | "stop" | "rebuild", instance: HermesInstance) {
    if (action !== "stop" && !requiredModelsReady) {
      setError(modelGateMessage);
      return;
    }
    setPendingHermesAction({ userId: instance.user_id, action });
    setError(null);
    try {
      if (action === "start") {
        await apiClient.startHermesInstance(instance.user_id);
      } else if (action === "stop") {
        await apiClient.stopHermesInstance(instance.user_id);
      } else {
        await apiClient.rebuildHermesInstance(instance.user_id);
      }
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.requestFailed"));
    } finally {
      setPendingHermesAction(null);
    }
  }

  async function createManagedHermes(userId: string) {
    if (!requiredModelsReady) {
      setError(modelGateMessage);
      return;
    }
    setPendingHermesAction({ userId, action: "create" });
    setError(null);
    try {
      await apiClient.createHermesInstance(userId);
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.requestFailed"));
    } finally {
      setPendingHermesAction(null);
    }
  }

  async function saveSystemParameters(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!systemSettingsLoaded) {
      return;
    }
    setSettingsSaved(false);
    setError(null);
    try {
      const submittedSettings = {
        max_sessions_per_user: systemSettings.max_sessions_per_user,
        max_attachment_upload_bytes: systemSettings.max_attachment_upload_bytes,
        attachment_retention_days: systemSettings.attachment_retention_days,
        empty_chat_prompt: systemSettings.empty_chat_prompt.trim(),
        speech_input: systemSettings.speech_input,
      };
      await apiClient.updateSystemParameters(submittedSettings);
      setSystemSettings((current) => ({
        ...current,
        empty_chat_prompt: submittedSettings.empty_chat_prompt,
      }));
      setSettingsSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function saveAuthSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!systemSettingsLoaded) {
      return;
    }
    setSettingsSaved(false);
    setError(null);
    try {
      await apiClient.updateAuthSettings({
        oidc: systemSettings.oidc,
        ldap: systemSettings.ldap,
      });
      setSettingsSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function savePublicPlatformSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!systemSettingsLoaded) {
      return;
    }
    setSettingsSaved(false);
    setError(null);
    try {
      await apiClient.updatePublicPlatformSettings({
        public_platform: systemSettings.public_platform,
      });
      const [nextPublicPlatformHermesStatus, nextPublicSessionsPage] = await Promise.all([
        apiClient.publicPlatformHermesInstance(),
        fetchPublicPlatformSessionsPage(publicSessionsPage.page),
      ]);
      setPublicPlatformHermesStatus(nextPublicPlatformHermesStatus);
      setPublicSessionsPage(nextPublicSessionsPage);
      setSettingsSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function saveApiManagementSettings(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!systemSettingsLoaded) {
      return;
    }
    setSettingsSaved(false);
    setError(null);
    try {
      await apiClient.updateApiManagementSettings({
        api_management: systemSettings.api_management,
      });
      setApiManagementEnabledForTabs(systemSettings.api_management.enabled);
      if (!systemSettings.api_management.enabled) {
        closeIntegrationAppDialog();
      }
      setSettingsSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function rebuildPublicPlatformHermes() {
    if (!requiredModelsReady) {
      setError(modelGateMessage);
      return;
    }
    setPublicHermesRebuilding(true);
    setError(null);
    try {
      await apiClient.rebuildPublicPlatformHermesInstance();
      setPublicPlatformHermesStatus(await apiClient.publicPlatformHermesInstance());
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.requestFailed"));
    } finally {
      setPublicHermesRebuilding(false);
    }
  }

  async function loadPublicPlatformSessions(page: number) {
    setPublicSessionsLoading(true);
    setError(null);
    try {
      const nextPage = await fetchPublicPlatformSessionsPage(page);
      setPublicSessionsPage(nextPage);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.workspaceLoadFailed"));
    } finally {
      setPublicSessionsLoading(false);
    }
  }

  async function forceClearPublicSession(session: PublicPlatformSessionSummary) {
    setForceClearingPublicSessionId(session.id);
    setError(null);
    try {
      await apiClient.forceClearPublicPlatformSession(session.id);
      const fallbackPage =
        publicSessionsPage.page > 1 && publicSessionsPage.sessions.length <= 1
          ? publicSessionsPage.page - 1
          : publicSessionsPage.page;
      await loadPublicPlatformSessions(fallbackPage);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.publicSessionClearFailed"));
    } finally {
      setForceClearingPublicSessionId(null);
    }
  }

  async function saveHermesProfile(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setHermesProfileSaved(false);
    setError(null);
    try {
      await apiClient.updateHermesProfile(hermesProfile);
      // 保存后重新读取一次，确保页面展示的是后端最终落库内容。
      setHermesProfile(await apiClient.hermesProfile());
      setHermesProfileSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.hermesProfileSaveFailed"));
    }
  }

  function updateLdapSettings(patch: Partial<SystemSettings["ldap"]>) {
    setSystemSettings((current) => ({
      ...current,
      ldap: { ...current.ldap, ...patch },
    }));
  }

  function updateApiManagementSettings(patch: Partial<SystemSettings["api_management"]>) {
    setSystemSettings((current) => ({
      ...current,
      api_management: { ...current.api_management, ...patch },
    }));
  }

  function sortIntegrationApps(nextIntegrationApps: IntegrationApp[]) {
    return [...nextIntegrationApps].sort((left, right) =>
      left.name.localeCompare(right.name),
    );
  }

  async function loadIntegrationApps() {
    const nextIntegrationApps = await apiClient.listIntegrationApps();
    setIntegrationApps(sortIntegrationApps(nextIntegrationApps));
  }

  function resetIntegrationAppForm() {
    setSettingsSaved(false);
    setError(null);
    selectedIntegrationAppIdRef.current = null;
    setSelectedIntegrationAppId(null);
    setIntegrationAppForm(blankIntegrationAppForm());
    setIntegrationAppSecret(null);
    setIntegrationAppSaved(false);
    setIntegrationTools([]);
    setIntegrationToolsLoading(false);
  }

  function openNewIntegrationAppDialog() {
    resetIntegrationAppForm();
    setIntegrationAppDialogOpen(true);
  }

  function closeIntegrationAppDialog() {
    setIntegrationAppDialogOpen(false);
    resetIntegrationAppForm();
  }

  async function loadIntegrationAppTools(appId: string) {
    setIntegrationToolsLoading(true);
    try {
      const nextTools = await apiClient.listIntegrationAppTools(appId);
      if (selectedIntegrationAppIdRef.current !== appId) {
        return;
      }
      setIntegrationTools(nextTools);
    } catch (cause) {
      if (selectedIntegrationAppIdRef.current === appId) {
        setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
        setIntegrationTools([]);
      }
    } finally {
      if (selectedIntegrationAppIdRef.current === appId) {
        setIntegrationToolsLoading(false);
      }
    }
  }

  function selectIntegrationApp(app: IntegrationApp) {
    setError(null);
    setSettingsSaved(false);
    setIntegrationAppDialogOpen(true);
    selectedIntegrationAppIdRef.current = app.id;
    setSelectedIntegrationAppId(app.id);
    setIntegrationAppForm(integrationAppFormFromApp(app));
    setIntegrationAppSecret(null);
    setIntegrationAppSaved(false);
    setIntegrationTools([]);
    void loadIntegrationAppTools(app.id);
  }

  function updateIntegrationAppForm(patch: Partial<IntegrationAppForm>) {
    setIntegrationAppSaved(false);
    setIntegrationAppSecret(null);
    setIntegrationAppForm((current) => ({
      ...current,
      ...patch,
    }));
  }

  async function saveIntegrationApp(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setIntegrationAppSaved(false);
    setSettingsSaved(false);
    setError(null);
    const payload: CreateIntegrationAppInput = {
      name: integrationAppForm.name.trim(),
      enabled: integrationAppForm.enabled,
      redirect_uri: integrationAppForm.redirect_uri.trim(),
      scopes: integrationAppForm.scopes.trim(),
      authorization_code_ttl_seconds: integrationAppForm.authorization_code_ttl_seconds,
      hidden_session_idle_timeout_seconds:
        integrationAppForm.hidden_session_idle_timeout_seconds,
      default_tool_timeout_seconds: integrationAppForm.default_tool_timeout_seconds,
      max_tool_timeout_seconds: integrationAppForm.max_tool_timeout_seconds,
    };
    if (!isValidIntegrationAppRedirectUri(payload.redirect_uri)) {
      setError(t("admin.integrationAppRedirectUriInvalid"));
      return;
    }
    if (
      !isValidPositiveInteger(payload.authorization_code_ttl_seconds ?? Number.NaN) ||
      !isValidPositiveInteger(payload.hidden_session_idle_timeout_seconds ?? Number.NaN) ||
      !isValidPositiveInteger(payload.default_tool_timeout_seconds ?? Number.NaN) ||
      !isValidPositiveInteger(payload.max_tool_timeout_seconds ?? Number.NaN)
    ) {
      setError(t("admin.integrationAppNumericSettingsInvalid"));
      return;
    }
    if ((payload.default_tool_timeout_seconds ?? 0) > (payload.max_tool_timeout_seconds ?? 0)) {
      setError(t("admin.integrationAppToolTimeoutOrderInvalid"));
      return;
    }
    try {
      if (selectedIntegrationAppId) {
        const updatePayload: UpdateIntegrationAppInput = {
          name: payload.name,
          enabled: payload.enabled,
          redirect_uri: payload.redirect_uri,
          scopes: payload.scopes,
          authorization_code_ttl_seconds: payload.authorization_code_ttl_seconds ?? 1,
          hidden_session_idle_timeout_seconds:
            payload.hidden_session_idle_timeout_seconds ?? 1,
          default_tool_timeout_seconds: payload.default_tool_timeout_seconds ?? 1,
          max_tool_timeout_seconds: payload.max_tool_timeout_seconds ?? 1,
        };
        const app = await apiClient.updateIntegrationApp(selectedIntegrationAppId, updatePayload);
        setIntegrationApps((current) =>
          sortIntegrationApps(current.map((candidate) => (candidate.id === app.id ? app : candidate))),
        );
        selectedIntegrationAppIdRef.current = app.id;
        setSelectedIntegrationAppId(app.id);
        setIntegrationAppForm(integrationAppFormFromApp(app));
        setIntegrationAppSecret(null);
      } else {
        const created = await apiClient.createIntegrationApp(payload);
        setIntegrationApps((current) =>
          sortIntegrationApps([...current.filter((candidate) => candidate.id !== created.app.id), created.app]),
        );
        selectedIntegrationAppIdRef.current = created.app.id;
        setSelectedIntegrationAppId(created.app.id);
        setIntegrationAppForm(integrationAppFormFromApp(created.app));
        setIntegrationAppSecret(created.client_secret);
        setIntegrationTools([]);
      }
      setIntegrationAppSaved(true);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function rotateIntegrationAppSecret() {
    if (!selectedIntegrationApp) {
      return;
    }
    setSettingsSaved(false);
    setError(null);
    try {
      const rotated = await apiClient.rotateIntegrationAppSecret(selectedIntegrationApp.id);
      setIntegrationApps((current) =>
        sortIntegrationApps(
          current.map((candidate) => (candidate.id === rotated.app.id ? rotated.app : candidate)),
        ),
      );
      setIntegrationAppSecret(rotated.client_secret);
      setIntegrationAppForm(integrationAppFormFromApp(rotated.app));
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function deleteIntegrationAppFromList(app: IntegrationApp) {
    const confirmed = window.confirm(t("admin.integrationAppDeleteConfirm"));
    if (!confirmed) {
      return;
    }
    setSettingsSaved(false);
    setError(null);
    try {
      await apiClient.deleteIntegrationApp(app.id);
      setIntegrationApps((current) => current.filter((candidate) => candidate.id !== app.id));
      if (selectedIntegrationAppIdRef.current === app.id) {
        closeIntegrationAppDialog();
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.settingsSaveFailed"));
    }
  }

  async function openManagedSkill(path: string) {
    setSkillSaved(false);
    setSkillLoading(true);
    setError(null);
    try {
      const skill = await apiClient.readManagedSkill(path);
      setSelectedSkillNode({ path: skill.path, kind: "file" });
      setSkillPathInput(skill.path);
      setSkillContent(skill.content);
      setSkillEditorMode("file");
    } catch (cause) {
      // 二进制或压缩包不是可编辑 Skill 文本，但仍要允许管理员选中后删除。
      setSelectedSkillNode({ path, kind: "file" });
      setSkillPathInput(path);
      setSkillContent("");
      setSkillEditorMode("file");
      setError(cause instanceof Error ? cause.message : t("admin.skillLoadFailed"));
    } finally {
      setSkillLoading(false);
    }
  }

  function selectManagedSkillDirectory(path: string) {
    setSkillSaved(false);
    setError(null);
    setSelectedSkillNode({ path, kind: "dir" });
    setSkillPathInput(path);
    setSkillContent("");
    setSkillEditorMode("directory");
  }

  function newManagedSkill() {
    const directory =
      selectedSkillNode?.kind === "dir"
        ? selectedSkillNode.path
        : selectedSkillNode?.path
          ? parentPath(selectedSkillNode.path)
          : "";
    setSelectedSkillNode(null);
    setSkillPathInput(defaultFilePathForDirectory(directory));
    setSkillContent("");
    setSkillEditorMode("file");
    setSkillSaved(false);
    setError(null);
  }

  function newManagedSkillDirectory() {
    const directory =
      selectedSkillNode?.kind === "dir"
        ? selectedSkillNode.path
        : selectedSkillNode?.path
          ? parentPath(selectedSkillNode.path)
          : "";
    setSelectedSkillNode(null);
    setSkillPathInput(defaultChildDirectoryPath(directory));
    setSkillContent("");
    setSkillEditorMode("directory");
    setSkillSaved(false);
    setError(null);
  }

  async function saveManagedSkill(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setSkillSaved(false);
    setSkillLoading(true);
    setError(null);
    try {
      const path = selectedSkillNode?.path ?? skillPathInput.trim();
      if (!path) {
        setError(t("admin.skillPathRequired"));
        return;
      }
      if (skillEditorMode === "directory") {
        if (selectedSkillNode?.kind === "dir" && selectedSkillNode.path !== path) {
          const fromPrefix = `${selectedSkillNode.path}/`;
          const selectedTreeNode = managedSkillTree
            ? findManagedSkillTreeNode(managedSkillTree, selectedSkillNode.path)
            : null;
          const directoriesToMove = selectedTreeNode
            ? collectManagedSkillDirectories(selectedTreeNode)
            : [selectedSkillNode.path];
          const filesToMove = managedSkills.filter(
            (skill) => skill.path === selectedSkillNode.path || skill.path.startsWith(fromPrefix),
          );
          for (const skill of filesToMove) {
            const suffix = skill.path.slice(selectedSkillNode.path.length).replace(/^\//, "");
            const target = suffix ? `${path}/${suffix}` : path;
            const content = await apiClient.readManagedSkill(skill.path);
            await apiClient.saveManagedSkill(target, content.content);
          }
          for (const directory of directoriesToMove) {
            const suffix = directory.slice(selectedSkillNode.path.length).replace(/^\//, "");
            await apiClient.createManagedSkillDirectory(suffix ? `${path}/${suffix}` : path);
          }
          await apiClient.deleteManagedSkill(selectedSkillNode.path);
          setSelectedSkillNode({ path, kind: "dir" });
          setSkillPathInput(path);
          setSkillContent("");
          setSkillSaved(true);
          await refreshManagedSkills();
          return;
        }
        await apiClient.createManagedSkillDirectory(path);
        setSelectedSkillNode({ path, kind: "dir" });
        setSkillPathInput(path);
        setSkillContent("");
        setSkillSaved(true);
        await refreshManagedSkills();
        return;
      }
      const saved = await apiClient.saveManagedSkill(path, skillContent);
      if (selectedSkillNode?.kind === "file" && selectedSkillNode.path !== saved.path) {
        await apiClient.deleteManagedSkill(selectedSkillNode.path);
      }
      setSelectedSkillNode({ path: saved.path, kind: "file" });
      setSkillPathInput(saved.path);
      setSkillContent(saved.content);
      setSkillSaved(true);
      await refreshManagedSkills();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.skillSaveFailed"));
    } finally {
      setSkillLoading(false);
    }
  }

  async function deleteManagedSkill() {
    const path = selectedSkillNode?.path ?? skillPathInput.trim();
    if (!path) {
      return;
    }
    setSkillSaved(false);
    setSkillLoading(true);
    setError(null);
    try {
      await apiClient.deleteManagedSkill(path);
      setSelectedSkillNode(null);
      setSkillPathInput("");
      setSkillContent("");
      setSkillEditorMode("file");
      await refreshManagedSkills();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.skillDeleteFailed"));
    } finally {
      setSkillLoading(false);
    }
  }

  async function uploadManagedSkillFiles(event: ChangeEvent<HTMLInputElement>) {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    if (files.length === 0) {
      return;
    }
    const targetPath = selectedSkillNode?.kind === "dir" ? selectedSkillNode.path : undefined;
    setSkillSaved(false);
    setSkillLoading(true);
    setError(null);
    try {
      await apiClient.uploadManagedSkills(files, targetPath);
      setSkillSaved(true);
      await refreshManagedSkills();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("admin.skillUploadFailed"));
    } finally {
      setSkillLoading(false);
    }
  }

  const missingRequiredModelNames =
    missingRequiredModels.length > 0
      ? missingRequiredModels.map((kind) => modelLabels[kind]).join(language === "zh" ? "、" : ", ")
      : [modelLabels.llm, modelLabels.title].join(language === "zh" ? "、" : ", ");
  const modelGateMessage = t("admin.modelGate", {
    models: missingRequiredModelNames,
  });
  const skillPathSeparator = language === "zh" ? "：" : ": ";

  function renderManagedSkillNode(node: ManagedSkillTreeNode): ReactNode {
    if (node.path === "") {
      return node.children.map(renderManagedSkillNode);
    }
    if (hasHiddenManagedSkillSegment(node.path)) {
      return null;
    }
    const selected = selectedSkillNode?.path === node.path && selectedSkillNode.kind === node.kind;
    return (
      <li key={`${node.kind}:${node.path}`} className={selected ? "selected" : undefined}>
        <button
          type="button"
          className={`skill-tree-button ${node.kind === "dir" ? "directory" : "file"}`}
          aria-label={node.name}
          data-managed-skill-path={node.path}
          title={node.path}
          onClick={() =>
            node.kind === "dir"
              ? selectManagedSkillDirectory(node.path)
              : void openManagedSkill(node.path)
          }
        >
          {node.kind === "dir" ? (
            <Folder className="skill-tree-icon" size={15} aria-hidden="true" />
          ) : (
            <FileText className="skill-tree-icon" size={15} aria-hidden="true" />
          )}
          <span className="skill-tree-name">{node.name}</span>
          {node.kind === "file" ? (
            <span className="skill-tree-size">{formatBytes(node.size)}</span>
          ) : null}
        </button>
        {node.kind === "dir" && node.children.length > 0 ? (
          <ul className="skill-tree">{node.children.map(renderManagedSkillNode)}</ul>
        ) : null}
      </li>
    );
  }

  function renderSystemSettingsShell(content: ReactNode) {
    return (
      <section className="admin-page" id="admin-settings">
        <div className="panel-heading">
          <h1>{t("admin.systemSettings")}</h1>
        </div>
        <div className="settings-tabs" role="tablist" aria-label={t("admin.systemSettings")}>
          {adminSettingsTabs.map((tab) => (
            <button
              type="button"
              key={tab.key}
              role="tab"
              aria-selected={activeTab === tab.key}
              className={activeTab === tab.key ? "active" : ""}
              onClick={() => selectAdminTab(tab.key)}
            >
              {tab.label}
            </button>
          ))}
        </div>
        {content}
      </section>
    );
  }

  if (activeTab === "models") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-models">
        <form className="admin-page" onSubmit={(event) => void saveModels(event)}>
          <div className="tab-actions">
            <div className="button-row">
              <button type="button" className="secondary" onClick={() => void refresh()}>
                {t("admin.refresh")}
              </button>
              <button type="submit">{t("admin.save")}</button>
            </div>
          </div>
          {error ? <p className="error">{error}</p> : null}
          {modelSaved ? <p className="copy-line">{t("admin.modelSaved")}</p> : null}
          <div className="model-config-grid">
            {orderedModelConfigs.map((config) => {
              const primaryTestKey = modelTestKey(config.config_kind, "primary");
              const primaryTestMessage = modelTestMessages[primaryTestKey];
              return (
                <section className="panel" key={config.config_kind}>
                  <div className="model-card-heading">
                    <h2>{modelLabels[config.config_kind]}</h2>
                    <button
                      type="button"
                      className="secondary"
                      disabled={testingModel === primaryTestKey}
                      onClick={() => void testModel(config)}
                    >
                      {t("admin.test")}
                    </button>
                  </div>
                  {primaryTestMessage ? (
                    <p
                      className={
                        primaryTestMessage === "model test succeeded" ? "copy-line" : "notice"
                      }
                    >
                      {primaryTestMessage}
                    </p>
                  ) : null}
                  <div className="form">
                    <label>
                      {t("admin.provider")}
                      <input
                        value={config.provider_name}
                        onChange={(event) =>
                          updateModel(config.config_kind, {
                            provider_name: event.target.value,
                          })
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
                          updateModel(config.config_kind, {
                            provider_api_key: event.target.value,
                          })
                        }
                      />
                    </label>
                    <label>
                      {t("admin.model")}
                      <input
                        value={config.default_model}
                        onChange={(event) =>
                          updateModel(config.config_kind, {
                            default_model: event.target.value,
                          })
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
                    {config.config_kind === "llm" ? (
                      <>
                        <label>
                          {t("admin.contextWindowTokens")}
                          <input
                            type="number"
                            min={1}
                            value={config.context_window_tokens}
                            onChange={(event) =>
                              updateModel(config.config_kind, {
                                context_window_tokens: Number(event.target.value),
                              })
                            }
                          />
                        </label>
                        <label>
                          {t("admin.maxOutputTokens")}
                          <input
                            type="number"
                            min={1}
                            value={config.max_output_tokens}
                            onChange={(event) =>
                              updateModel(config.config_kind, {
                                max_output_tokens: Number(event.target.value),
                              })
                            }
                          />
                        </label>
                        <label>
                          {t("admin.temperature")}
                          <input
                            type="number"
                            min={0}
                            max={2}
                            step={0.1}
                            value={config.temperature}
                            onChange={(event) =>
                              updateModel(config.config_kind, {
                                temperature: Number(event.target.value),
                              })
                            }
                          />
                        </label>
                      </>
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
                    {config.config_kind === "llm" ? (
                      <label className="checkbox-row">
                        <input
                          type="checkbox"
                          checked={config.supports_parallel_tools}
                          onChange={(event) =>
                            updateModel(config.config_kind, {
                              supports_parallel_tools: event.target.checked,
                            })
                          }
                        />
                        {t("admin.supportsParallelTools")}
                      </label>
                    ) : null}
                    {config.config_kind !== "image"
                      ? (() => {
                          const fallback = fallbackConfigForModel(config);
                          const fallbackTestKey = modelTestKey(config.config_kind, "fallback");
                          const fallbackTestMessage = modelTestMessages[fallbackTestKey];
                          return (
                            <fieldset className="form-section model-fallback-section">
                              <legend>{t("admin.fallbackModel")}</legend>
                              <label className="checkbox-row">
                                <input
                                  type="checkbox"
                                  checked={fallback.enabled}
                                  onChange={(event) =>
                                    updateModelFallback(config.config_kind, {
                                      enabled: event.target.checked,
                                    })
                                  }
                                />
                                {t("admin.fallbackEnabled")}
                              </label>
                              {fallback.enabled ? (
                                <>
                                  <div className="button-row">
                                    <button
                                      type="button"
                                      className="secondary"
                                      disabled={testingModel === fallbackTestKey}
                                      onClick={() => void testModel(config, "fallback")}
                                    >
                                      {t("admin.testFallback")}
                                    </button>
                                  </div>
                                  {fallbackTestMessage ? (
                                    <p
                                      className={
                                        fallbackTestMessage === "model test succeeded"
                                          ? "copy-line"
                                          : "notice"
                                      }
                                    >
                                      {fallbackTestMessage}
                                    </p>
                                  ) : null}
                                  <label>
                                    {t("admin.fallbackProvider")}
                                    <input
                                      value={fallback.provider_name}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
                                          provider_name: event.target.value,
                                        })
                                      }
                                    />
                                  </label>
                                  <label>
                                    {t("admin.fallbackBaseUrl")}
                                    <input
                                      value={fallback.provider_base_url}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
                                          provider_base_url: event.target.value,
                                        })
                                      }
                                    />
                                  </label>
                                  <label>
                                    {t("admin.fallbackApiKey")}
                                    <input
                                      type="password"
                                      value={fallback.provider_api_key ?? ""}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
                                          provider_api_key: event.target.value,
                                        })
                                      }
                                    />
                                  </label>
                                  <label>
                                    {t("admin.fallbackModelName")}
                                    <input
                                      value={fallback.default_model}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
                                          default_model: event.target.value,
                                        })
                                      }
                                    />
                                  </label>
                                  <label>
                                    {t("admin.fallbackApi")}
                                    <select
                                      value={fallback.api_type}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
                                          api_type: event.target.value as ModelApiType,
                                        })
                                      }
                                    >
                                      {["chat_completions", "responses"].map((apiType) => (
                                        <option key={apiType} value={apiType}>
                                          {apiTypeLabels[apiType as ModelApiType]}
                                        </option>
                                      ))}
                                    </select>
                                  </label>
                                  <label>
                                    {t("admin.fallbackReasoning")}
                                    <select
                                      value={fallback.reasoning_effort ?? ""}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
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
                                  {config.config_kind === "llm" ? (
                                    <>
                                      <label>
                                        {t("admin.fallbackContextWindowTokens")}
                                        <input
                                          type="number"
                                          min={1}
                                          value={fallback.context_window_tokens}
                                          onChange={(event) =>
                                            updateModelFallback(config.config_kind, {
                                              context_window_tokens: Number(event.target.value),
                                            })
                                          }
                                        />
                                      </label>
                                      <label>
                                        {t("admin.fallbackMaxOutputTokens")}
                                        <input
                                          type="number"
                                          min={1}
                                          value={fallback.max_output_tokens}
                                          onChange={(event) =>
                                            updateModelFallback(config.config_kind, {
                                              max_output_tokens: Number(event.target.value),
                                            })
                                          }
                                        />
                                      </label>
                                      <label>
                                        {t("admin.fallbackTemperature")}
                                        <input
                                          type="number"
                                          min={0}
                                          max={2}
                                          step={0.1}
                                          value={fallback.temperature}
                                          onChange={(event) =>
                                            updateModelFallback(config.config_kind, {
                                              temperature: Number(event.target.value),
                                            })
                                          }
                                        />
                                      </label>
                                    </>
                                  ) : null}
                                  <label>
                                    {t("admin.fallbackTimeout")}
                                    <input
                                      type="number"
                                      min={1}
                                      value={fallback.request_timeout_seconds}
                                      onChange={(event) =>
                                        updateModelFallback(config.config_kind, {
                                          request_timeout_seconds: Number(event.target.value),
                                        })
                                      }
                                    />
                                  </label>
                                  {config.config_kind === "llm" ? (
                                    <label className="checkbox-row">
                                      <input
                                        type="checkbox"
                                        checked={fallback.allow_streaming}
                                        onChange={(event) =>
                                          updateModelFallback(config.config_kind, {
                                            allow_streaming: event.target.checked,
                                          })
                                        }
                                      />
                                      {t("admin.fallbackStreaming")}
                                    </label>
                                  ) : null}
                                  {config.config_kind === "llm" ? (
                                    <label className="checkbox-row">
                                      <input
                                        type="checkbox"
                                        checked={fallback.supports_parallel_tools}
                                        onChange={(event) =>
                                          updateModelFallback(config.config_kind, {
                                            supports_parallel_tools: event.target.checked,
                                          })
                                        }
                                      />
                                      {t("admin.fallbackSupportsParallelTools")}
                                    </label>
                                  ) : null}
                                </>
                              ) : null}
                            </fieldset>
                          );
                        })()
                      : null}
                    {config.config_kind === "image" ? (
                      <label className="checkbox-row">
                        <input
                          type="checkbox"
                          checked={config.enabled}
                          onChange={(event) =>
                            updateModel(config.config_kind, {
                              enabled: event.target.checked,
                            })
                          }
                        />
                        {t("admin.imageEnabled")}
                      </label>
                    ) : null}
                  </div>
                </section>
              );
            })}
          </div>
        </form>
      </section>,
    );
  }

  if (activeTab === "hermes") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-hermes">
        <div className="tab-actions">
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
                <th>{t("admin.startedAt")}</th>
                <th>{t("admin.stopTime")}</th>
                <th>{t("admin.version")}</th>
                <th>{t("admin.action")}</th>
              </tr>
            </thead>
            <tbody>
              {users.map((owner) => {
                const instance = instancesByUserId.get(owner.id);
                const statusDisplay = hermesInstanceStatusDisplay(instance);
                return (
                  <tr key={owner.id}>
                    <td>{owner.email}</td>
                    <td>{instance?.kind ?? "not_created"}</td>
                    <td>
                      <span className="status-cell">
                        <span>{statusDisplay.label}</span>
                        {statusDisplay.detail ? (
                          <span className="status-detail">{statusDisplay.detail}</span>
                        ) : null}
                      </span>
                    </td>
                    <td>{formatHermesStartedAt(instance, language)}</td>
                    <td>{formatHermesStopTime(instance, language, t)}</td>
                    <td title={instance?.runtime_image ?? undefined}>
                      {formatHermesRuntimeVersion(instance)}
                    </td>
                    <td>
                      {!instance ? (
                        <button
                          type="button"
                          className="secondary"
                          disabled={!requiredModelsReady || isHermesActionPending(owner.id)}
                          onClick={() => void createManagedHermes(owner.id)}
                        >
                          {hermesActionLabel(owner.id, "create", "admin.create")}
                        </button>
                      ) : (
                        <div className="button-row">
                          <button
                            type="button"
                            className="secondary"
                            disabled={!requiredModelsReady || isHermesActionPending(owner.id)}
                            onClick={() => void controlInstance("start", instance)}
                          >
                            {hermesActionLabel(owner.id, "start", "admin.start")}
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            disabled={isHermesActionPending(owner.id)}
                            onClick={() => void controlInstance("stop", instance)}
                          >
                            {hermesActionLabel(owner.id, "stop", "admin.stop")}
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            disabled={!requiredModelsReady || isHermesActionPending(owner.id)}
                            onClick={() => void controlInstance("rebuild", instance)}
                          >
                            {hermesActionLabel(owner.id, "rebuild", "admin.rebuild")}
                          </button>
                        </div>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>,
    );
  }

  if (activeTab === "profile") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-hermes-profile">
        <form
          className="panel form hermes-profile-editor"
          onSubmit={(event) => void saveHermesProfile(event)}
        >
          <div className="tab-actions">
            <div className="button-row">
              <button type="button" className="secondary" onClick={() => void refresh()}>
                {t("admin.refresh")}
              </button>
              <button type="submit">{t("admin.save")}</button>
            </div>
          </div>
          {error ? <p className="error">{error}</p> : null}
          {hermesProfileSaved ? <p className="copy-line">{t("admin.hermesProfileSaved")}</p> : null}
          <MarkdownVditorEditor
            className="soul-vditor-editor"
            label={t("admin.soulMd")}
            value={hermesProfile.soul_md}
            onChange={(soulMd) => {
              setHermesProfile({ soul_md: soulMd });
              setHermesProfileSaved(false);
            }}
          />
        </form>
      </section>,
    );
  }

  if (activeTab === "scheduler") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-scheduler">
        <div className="tab-actions">
          <button type="button" className="secondary" onClick={() => void refresh()}>
            {t("admin.refresh")}
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        <div className="panel scheduler-panel">
          {schedulerTaskRows.length === 0 ? (
            <div className="empty-state">
              <strong>{t("admin.noScheduledTasks")}</strong>
            </div>
          ) : (
            <div className="table-scroll">
              <table aria-label={t("admin.scheduledTasks")}>
                <thead>
                  <tr>
                    <th>{t("admin.owner")}</th>
                    <th>{t("admin.schedulerTask")}</th>
                    <th>{t("admin.schedule")}</th>
                    <th>{t("admin.nextRunAt")}</th>
                    <th>{t("admin.lastRunAt")}</th>
                    <th>{t("admin.status")}</th>
                    <th>{t("admin.source")}</th>
                    <th>{t("admin.instanceStatus")}</th>
                    <th>{t("admin.reportedAt")}</th>
                  </tr>
                </thead>
                <tbody>
                  {schedulerTaskRows.map(({ snapshot, task }) => (
                    <tr key={`${snapshot.hermes_instance_id}:${task.id}`}>
                      <td>
                        <span className="status-cell">
                          <span>
                            {snapshot.user_email?.trim() ||
                              usersById.get(snapshot.user_id)?.email ||
                              snapshot.user_id}
                          </span>
                          <span className="status-detail">{snapshot.user_id}</span>
                        </span>
                      </td>
                      <td>
                        <span className="status-cell">
                          <span>{task.name || task.id}</span>
                          <span className="status-detail">
                            {task.enabled ? t("admin.enabled") : t("admin.disabled")} / {task.id}
                          </span>
                        </span>
                      </td>
                      <td>
                        <span className="status-cell">
                          <span>{task.schedule || "-"}</span>
                          <span className="status-detail">{task.timezone || "-"}</span>
                        </span>
                      </td>
                      <td>{formatSchedulerSnapshotTime(task.next_run_at, language)}</td>
                      <td>{formatSchedulerSnapshotTime(task.last_run_at, language)}</td>
                      <td>{task.status || "-"}</td>
                      <td>{task.source || "-"}</td>
                      <td>
                        <span className="status-cell">
                          <span>{snapshot.instance_status || "-"}</span>
                          <span className="status-detail">
                            {snapshot.scheduler_enabled
                              ? t("admin.schedulerEnabled")
                              : t("admin.schedulerDisabled")}
                            {" / "}
                            {t("admin.runningJobs")}: {snapshot.running_jobs_count}
                          </span>
                          <span className="status-detail">{snapshot.hermes_instance_id}</span>
                        </span>
                      </td>
                      <td>{formatSchedulerSnapshotTime(snapshot.reported_at, language)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
      </section>,
    );
  }

  if (activeTab === "system") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-system-parameters">
        <form className="panel form" onSubmit={(event) => void saveSystemParameters(event)}>
          <div className="tab-actions">
            <button type="button" className="secondary" onClick={() => void refresh()}>
              {t("admin.refresh")}
            </button>
          </div>
          {error ? <p className="error">{error}</p> : null}
          {settingsSaved ? <p className="copy-line">{t("admin.settingsSaved")}</p> : null}
          <label>
            {t("admin.emptyChatPrompt")}
            <textarea
              rows={3}
              value={systemSettings.empty_chat_prompt}
              placeholder={t("chat.empty")}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  empty_chat_prompt: event.target.value,
                })
              }
            />
          </label>
          <label>
            {t("admin.maxSessionsPerUser")}
            <input
              type="number"
              min={1}
              max={500}
              value={systemSettings.max_sessions_per_user}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  max_sessions_per_user: Number(event.target.value),
                })
              }
              required
            />
          </label>
          <label>
            {t("admin.maxAttachmentUploadMegabytes")}
            <input
              type="number"
              min={1}
              value={megabytesFromBytes(systemSettings.max_attachment_upload_bytes)}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  max_attachment_upload_bytes: bytesFromMegabytes(Number(event.target.value)),
                })
              }
              required
            />
          </label>
          <label>
            {t("admin.attachmentRetentionDays")}
            <input
              type="number"
              min={1}
              max={3650}
              value={systemSettings.attachment_retention_days}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  attachment_retention_days: Number(event.target.value),
                })
              }
              required
            />
          </label>
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={systemSettings.speech_input.enabled}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  speech_input: {
                    ...systemSettings.speech_input,
                    enabled: event.target.checked,
                  },
                })
              }
            />
            {t("admin.speechInputEnabled")}
          </label>
          <p className={speechInputRuntimeConfig.runtime_available ? "copy-line" : "error"}>
            {speechInputRuntimeConfig.runtime_available
              ? t("admin.speechInputRuntimeAvailable")
              : t("admin.speechInputRuntimeUnavailable")}
          </p>
          <div className="button-row">
            <button type="submit" disabled={!systemSettingsLoaded}>
              {t("admin.saveSettings")}
            </button>
          </div>
        </form>
      </section>,
    );
  }

  if (activeTab === "public-platform") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-public-platform">
        <form className="panel form" onSubmit={(event) => void savePublicPlatformSettings(event)}>
          <div className="tab-actions">
            <button type="button" className="secondary" onClick={() => void refresh()}>
              {t("admin.refresh")}
            </button>
          </div>
          {error ? <p className="error">{error}</p> : null}
          {settingsSaved ? <p className="copy-line">{t("admin.settingsSaved")}</p> : null}
          <label className="checkbox-row">
            <input
              type="checkbox"
              checked={systemSettings.public_platform.enabled}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  public_platform: {
                    ...systemSettings.public_platform,
                    enabled: event.target.checked,
                  },
                })
              }
            />
            {t("admin.enable")}
          </label>
          <label>
            {t("admin.publicTemporarySessionRetentionHours")}
            <input
              type="number"
              min={1}
              max={8760}
              value={systemSettings.public_platform.temporary_session_retention_hours}
              onChange={(event) =>
                setSystemSettings({
                  ...systemSettings,
                  public_platform: {
                    ...systemSettings.public_platform,
                    temporary_session_retention_hours: Number(event.target.value),
                  },
                })
              }
              required
            />
          </label>
          <div className="public-hermes-status" aria-label={t("admin.publicHermes")}>
            <div className="section-heading-row">
              <strong>{t("admin.publicHermes")}</strong>
              <button
                type="button"
                className="secondary"
                disabled={
                  !requiredModelsReady ||
                  !publicPlatformHermesStatus.enabled ||
                  publicHermesRebuilding
                }
                onClick={() => void rebuildPublicPlatformHermes()}
              >
                {publicHermesRebuilding
                  ? t("admin.rebuilding")
                  : t("admin.rebuildPublicHermes")}
              </button>
            </div>
            {!requiredModelsReady ? <p className="notice">{modelGateMessage}</p> : null}
            <dl className="settings-detail-list">
              <div>
                <dt>{t("admin.enabled")}</dt>
                <dd>{publicPlatformHermesStatus.enabled ? t("admin.enabled") : t("admin.disabled")}</dd>
              </div>
              <div>
                <dt>{t("admin.ready")}</dt>
                <dd>{publicPlatformHermesStatus.ready ? t("admin.yes") : t("admin.no")}</dd>
              </div>
              <div>
                <dt>{t("admin.status")}</dt>
                <dd>
                  {(() => {
                    const instance = publicPlatformHermesStatus.hermes_instance ?? undefined;
                    const statusDisplay = hermesInstanceStatusDisplay(instance);
                    return (
                      <span className="status-cell">
                        <span>{statusDisplay.label}</span>
                        {statusDisplay.detail ? (
                          <span className="status-detail">{statusDisplay.detail}</span>
                        ) : null}
                      </span>
                    );
                  })()}
                </dd>
              </div>
              <div>
                <dt>{t("admin.startedAt")}</dt>
                <dd>
                  {formatHermesStartedAt(
                    publicPlatformHermesStatus.hermes_instance ?? undefined,
                    language,
                  )}
                </dd>
              </div>
              <div>
                <dt>{t("admin.version")}</dt>
                <dd title={publicPlatformHermesStatus.hermes_instance?.runtime_image ?? undefined}>
                  {formatHermesRuntimeVersion(
                    publicPlatformHermesStatus.hermes_instance ?? undefined,
                  )}
                </dd>
              </div>
            </dl>
          </div>
          <div className="public-sessions-panel" aria-label={t("admin.publicSessions")}>
            <div className="section-heading-row">
              <strong>{t("admin.publicSessions")}</strong>
              <button
                type="button"
                className="secondary"
                disabled={publicSessionsLoading || Boolean(forceClearingPublicSessionId)}
                onClick={() => void loadPublicPlatformSessions(publicSessionsPage.page)}
              >
                {t("admin.refresh")}
              </button>
            </div>
            {publicSessionsPage.sessions.length === 0 ? (
              <div className="empty-state">{t("admin.noPublicSessions")}</div>
            ) : (
              <div className="table-scroll">
                <table aria-label={t("admin.publicSessions")}>
                  <thead>
                    <tr>
                      <th scope="col">{t("admin.sessionTitle")}</th>
                      <th scope="col">{t("admin.createdAt")}</th>
                      <th scope="col">{t("admin.estimatedClearAt")}</th>
                      <th scope="col">{t("admin.publicSessionLink")}</th>
                      <th scope="col">{t("admin.action")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {publicSessionsPage.sessions.map((session) => {
                      const publicLink = publicPlatformSessionLink(session);
                      const clearing = forceClearingPublicSessionId === session.id;
                      return (
                        <tr key={session.id}>
                          <td>{session.title?.trim() || t("chat.newConversation")}</td>
                          <td>{formatSchedulerSnapshotTime(session.created_at, language)}</td>
                          <td>{formatSchedulerSnapshotTime(session.recycle_at, language)}</td>
                          <td>
                            <a
                              className="public-session-link"
                              href={publicLink}
                              target="_blank"
                              rel="noreferrer"
                            >
                              {publicLink}
                            </a>
                          </td>
                          <td>
                            <button
                              type="button"
                              className="secondary danger"
                              disabled={Boolean(forceClearingPublicSessionId)}
                              onClick={() => void forceClearPublicSession(session)}
                            >
                              {clearing ? t("admin.clearing") : t("admin.forceClear")}
                            </button>
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            )}
            {publicSessionsPage.total > 0 ? (
              <div className="pagination-row">
                <button
                  type="button"
                  className="secondary"
                  disabled={
                    publicSessionsLoading ||
                    Boolean(forceClearingPublicSessionId) ||
                    publicSessionsPage.page <= 1
                  }
                  onClick={() => void loadPublicPlatformSessions(publicSessionsPage.page - 1)}
                >
                  {t("admin.previousPage")}
                </button>
                <span aria-live="polite">
                  {t("admin.pageStatus", {
                    page: publicSessionsPage.page,
                    totalPages: Math.max(publicSessionsPage.total_pages, 1),
                    total: publicSessionsPage.total,
                  })}
                </span>
                <button
                  type="button"
                  className="secondary"
                  disabled={
                    publicSessionsLoading ||
                    Boolean(forceClearingPublicSessionId) ||
                    publicSessionsPage.page >= Math.max(publicSessionsPage.total_pages, 1)
                  }
                  onClick={() => void loadPublicPlatformSessions(publicSessionsPage.page + 1)}
                >
                  {t("admin.nextPage")}
                </button>
              </div>
            ) : null}
          </div>
          <div className="button-row">
            <button type="submit" disabled={!systemSettingsLoaded}>
              {t("admin.saveSettings")}
            </button>
          </div>
        </form>
      </section>,
    );
  }

  if (activeTab === "auth") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-auth-settings">
        <form className="panel form" onSubmit={(event) => void saveAuthSettings(event)}>
          <div className="tab-actions">
            <button type="button" className="secondary" onClick={() => void refresh()}>
              {t("admin.refresh")}
            </button>
          </div>
          {error ? <p className="error">{error}</p> : null}
          {settingsSaved ? <p className="copy-line">{t("admin.settingsSaved")}</p> : null}
          <fieldset className="form-section">
            <legend>{t("admin.oidcSettings")}</legend>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={systemSettings.oidc.enabled}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      enabled: event.target.checked,
                    },
                  })
                }
              />
              {t("admin.oidcEnabled")}
            </label>
            <label className="readonly-field">
              {t("admin.oidcRedirectUri")}
              <input readOnly value={oidcRedirectUri} />
            </label>
            <label>
              {t("admin.oidcDisplayName")}
              <input
                value={systemSettings.oidc.display_name}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      display_name: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcClientId")}
              <input
                value={systemSettings.oidc.client_id}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      client_id: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcClientSecret")}
              <input
                type="password"
                value={systemSettings.oidc.client_secret}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      client_secret: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcIssuerUrl")}
              <input
                value={systemSettings.oidc.issuer_url}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      issuer_url: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcAuthorizationUrl")}
              <input
                value={systemSettings.oidc.authorization_url}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      authorization_url: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcTokenUrl")}
              <input
                value={systemSettings.oidc.token_url}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      token_url: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcUserinfoUrl")}
              <input
                value={systemSettings.oidc.userinfo_url}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      userinfo_url: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcLogoutUrl")}
              <input
                value={systemSettings.oidc.logout_url}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      logout_url: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcScopes")}
              <input
                value={systemSettings.oidc.scopes}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      scopes: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcUsernameClaim")}
              <input
                value={systemSettings.oidc.username_claim}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      username_claim: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label>
              {t("admin.oidcEmailClaim")}
              <input
                value={systemSettings.oidc.email_claim}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      email_claim: event.target.value,
                    },
                  })
                }
              />
            </label>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={systemSettings.oidc.auto_create_users}
                onChange={(event) =>
                  setSystemSettings({
                    ...systemSettings,
                    oidc: {
                      ...systemSettings.oidc,
                      auto_create_users: event.target.checked,
                    },
                  })
                }
              />
              {t("admin.oidcAutoCreateUsers")}
            </label>
          </fieldset>
          <fieldset className="form-section">
            <legend>{t("admin.ldapSettings")}</legend>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={systemSettings.ldap.enabled}
                onChange={(event) => updateLdapSettings({ enabled: event.target.checked })}
              />
              {t("admin.ldapEnabled")}
            </label>
            <label>
              {t("admin.ldapDisplayName")}
              <input
                value={systemSettings.ldap.display_name}
                onChange={(event) => updateLdapSettings({ display_name: event.target.value })}
              />
            </label>
            <label>
              {t("admin.ldapUrl")}
              <input
                value={systemSettings.ldap.url}
                onChange={(event) => updateLdapSettings({ url: event.target.value })}
              />
            </label>
            <label>
              {t("admin.ldapBindDn")}
              <input
                value={systemSettings.ldap.bind_dn}
                onChange={(event) => updateLdapSettings({ bind_dn: event.target.value })}
              />
            </label>
            <label>
              {t("admin.ldapBindPassword")}
              <input
                type="password"
                value={systemSettings.ldap.bind_password}
                onChange={(event) => updateLdapSettings({ bind_password: event.target.value })}
              />
            </label>
            <label>
              {t("admin.ldapBaseDn")}
              <input
                value={systemSettings.ldap.base_dn}
                onChange={(event) => updateLdapSettings({ base_dn: event.target.value })}
              />
            </label>
            <label>
              {t("admin.ldapUserFilter")}
              <input
                value={systemSettings.ldap.user_filter}
                onChange={(event) => updateLdapSettings({ user_filter: event.target.value })}
              />
            </label>
            <label>
              {t("admin.ldapEmailAttribute")}
              <input
                value={systemSettings.ldap.email_attribute}
                onChange={(event) => updateLdapSettings({ email_attribute: event.target.value })}
              />
            </label>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={systemSettings.ldap.auto_create_users}
                onChange={(event) =>
                  updateLdapSettings({
                    auto_create_users: event.target.checked,
                  })
                }
              />
              {t("admin.ldapAutoCreateUsers")}
            </label>
          </fieldset>
          <div className="button-row">
            <button type="submit" disabled={!systemSettingsLoaded}>
              {t("admin.saveSettings")}
            </button>
          </div>
        </form>
      </section>,
    );
  }

  if (activeTab === "api-management") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-api-management">
        <div className="tab-actions">
          <button type="button" className="secondary" onClick={() => void refresh()}>
            {t("admin.refresh")}
          </button>
        </div>
        {error ? <p className="error">{error}</p> : null}
        {settingsSaved ? <p className="copy-line">{t("admin.settingsSaved")}</p> : null}
        <form className="panel form" onSubmit={(event) => void saveApiManagementSettings(event)}>
          <fieldset className="form-section">
            <legend>{t("admin.apiManagementSettings")}</legend>
            <label className="checkbox-row">
              <input
                type="checkbox"
                checked={systemSettings.api_management.enabled}
                onChange={(event) =>
                  updateApiManagementSettings({ enabled: event.target.checked })
                }
              />
              {t("admin.apiManagementEnabled")}
            </label>
            <p className="copy-line">{t("admin.apiManagementHint")}</p>
            <label className="readonly-field">
              {t("admin.apiDocsUrl")}
              <input readOnly value={apiDocsUrl} />
            </label>
            <label className="readonly-field">
              {t("admin.apiOpenApiUrl")}
              <input readOnly value={apiOpenApiUrl} />
            </label>
            <div className="button-row">
              <button
                type="button"
                className="secondary"
                disabled={!systemSettings.api_management.enabled}
                onClick={() => window.open(apiDocsUrl, "_blank", "noopener,noreferrer")}
              >
                <ExternalLink aria-hidden="true" size={16} />
                {t("admin.openApiDocs")}
              </button>
            </div>
          </fieldset>
          <div className="button-row">
            <button type="submit" disabled={!systemSettingsLoaded}>
              {t("admin.saveSettings")}
            </button>
          </div>
        </form>
      </section>,
    );
  }

  if (activeTab === "integration-apps") {
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-integration-apps">
        {error && !integrationAppDialogOpen ? <p className="error">{error}</p> : null}
        <div
          className="panel"
          aria-hidden={integrationAppDialogOpen}
          inert={integrationAppDialogOpen}
        >
          <div className="panel-heading">
            <h2>{t("admin.integrationApps")}</h2>
            <button type="button" className="secondary" onClick={openNewIntegrationAppDialog}>
              <Plus aria-hidden="true" size={16} />
              {t("admin.integrationAppNew")}
            </button>
          </div>
          <p className="notice">{t("admin.integrationAppSelectHint")}</p>
          {integrationApps.length === 0 ? (
            <p className="notice">{t("admin.integrationAppEmpty")}</p>
          ) : (
            <div className="table-scroll">
              <table>
                <thead>
                  <tr>
                    <th>{t("admin.integrationAppListName")}</th>
                    <th>{t("admin.integrationAppListIntegrationId")}</th>
                    <th>{t("admin.integrationAppListClientId")}</th>
                    <th>{t("admin.status")}</th>
                    <th>{t("admin.action")}</th>
                  </tr>
                </thead>
                <tbody>
                  {integrationApps.map((app) => (
                    <tr
                      key={app.id}
                      className={selectedIntegrationAppId === app.id ? "selected" : undefined}
                    >
                      <td>
                        <strong>{app.name}</strong>
                      </td>
                      <td>{app.integration_id}</td>
                      <td>{app.client_id}</td>
                      <td>{app.enabled ? t("admin.enabled") : t("admin.disabled")}</td>
                      <td>
                        <div className="button-row table-actions">
                          <button
                            type="button"
                            className="secondary"
                            aria-label={`${t("admin.edit")} ${app.name}`}
                            onClick={() => selectIntegrationApp(app)}
                          >
                            {t("admin.edit")}
                          </button>
                          <button
                            type="button"
                            className="secondary danger"
                            aria-label={`${t("admin.delete")} ${app.name}`}
                            onClick={() => void deleteIntegrationAppFromList(app)}
                          >
                            {t("admin.delete")}
                          </button>
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </div>
        {integrationAppDialogOpen ? (
          <div
            className="admin-modal"
            role="dialog"
            aria-modal="true"
            aria-label={
              selectedIntegrationApp
                ? t("admin.integrationAppDetails")
                : t("admin.integrationAppCreate")
            }
          >
            <button
              type="button"
              className="admin-modal-backdrop"
              aria-label={t("admin.closeDialog")}
              onClick={closeIntegrationAppDialog}
            />
            <div className="admin-modal-panel">
              <form
                className="form admin-modal-form"
                noValidate
                onSubmit={(event) => void saveIntegrationApp(event)}
              >
                <div className="admin-modal-header">
                  <div className="admin-modal-heading">
                    <h2>
                      {selectedIntegrationApp
                        ? t("admin.integrationAppDetails")
                        : t("admin.integrationAppCreate")}
                    </h2>
                    <p className="notice">
                      {selectedIntegrationApp
                        ? t("admin.integrationAppEditHint")
                        : t("admin.integrationAppCreateHint")}
                    </p>
                  </div>
                  <div className="button-row">
                    {selectedIntegrationApp ? (
                      <button
                        type="button"
                        className="secondary"
                        onClick={() => void rotateIntegrationAppSecret()}
                      >
                        <KeyRound aria-hidden="true" size={16} />
                        {t("admin.integrationAppRotateSecret")}
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="secondary icon-button"
                      aria-label={t("admin.closeDialog")}
                      onClick={closeIntegrationAppDialog}
                    >
                      <X aria-hidden="true" size={16} />
                    </button>
                  </div>
                </div>
                {error ? <p className="error">{error}</p> : null}
                {integrationAppSaved ? <p className="copy-line">{t("admin.integrationAppSaved")}</p> : null}
                {integrationAppSecret ? (
                  <p className="copy-line">
                    {t("admin.integrationAppSecretGenerated")}: {integrationAppSecret}
                  </p>
                ) : null}
                {selectedIntegrationApp ? (
                  <label className="readonly-field">
                    {t("admin.integrationAppClientId")}
                    <input readOnly value={selectedIntegrationApp.client_id} />
                  </label>
                ) : null}
                {selectedIntegrationApp ? (
                  <label className="readonly-field">
                    {t("admin.integrationAppIntegrationId")}
                    <input readOnly value={selectedIntegrationApp.integration_id} />
                  </label>
                ) : null}
                <label>
                  {t("admin.integrationAppName")}
                  <input
                    ref={integrationAppNameInputRef}
                    value={integrationAppForm.name}
                    onChange={(event) => updateIntegrationAppForm({ name: event.target.value })}
                    required
                  />
                </label>
                <label className="checkbox-row">
                  <input
                    type="checkbox"
                    checked={integrationAppForm.enabled}
                    onChange={(event) =>
                      updateIntegrationAppForm({ enabled: event.target.checked })
                    }
                  />
                  {t("admin.integrationAppEnabled")}
                </label>
                <label>
                  {t("admin.integrationAppRedirectUri")}
                  <input
                    type="url"
                    value={integrationAppForm.redirect_uri}
                    onChange={(event) =>
                      updateIntegrationAppForm({
                        redirect_uri: event.target.value,
                      })
                    }
                    required
                  />
                </label>
                <label>
                  {t("admin.integrationAppScopes")}
                  <input
                    value={integrationAppForm.scopes}
                    onChange={(event) => updateIntegrationAppForm({ scopes: event.target.value })}
                    required
                  />
                </label>
                <label>
                  {t("admin.integrationAppAuthorizationCodeTtlSeconds")}
                  <input
                    type="number"
                    min={1}
                    value={integrationAppForm.authorization_code_ttl_seconds}
                    onChange={(event) =>
                      updateIntegrationAppForm({
                        authorization_code_ttl_seconds: Number(event.target.value),
                      })
                    }
                    required
                  />
                </label>
                <label>
                  {t("admin.integrationAppHiddenSessionIdleTimeoutSeconds")}
                  <input
                    type="number"
                    min={1}
                    value={integrationAppForm.hidden_session_idle_timeout_seconds}
                    onChange={(event) =>
                      updateIntegrationAppForm({
                        hidden_session_idle_timeout_seconds: Number(event.target.value),
                      })
                    }
                    required
                  />
                </label>
                <label>
                  {t("admin.integrationAppDefaultToolTimeoutSeconds")}
                  <input
                    type="number"
                    min={1}
                    value={integrationAppForm.default_tool_timeout_seconds}
                    onChange={(event) =>
                      updateIntegrationAppForm({
                        default_tool_timeout_seconds: Number(event.target.value),
                      })
                    }
                    required
                  />
                </label>
                <label>
                  {t("admin.integrationAppMaxToolTimeoutSeconds")}
                  <input
                    type="number"
                    min={1}
                    value={integrationAppForm.max_tool_timeout_seconds}
                    onChange={(event) =>
                      updateIntegrationAppForm({
                        max_tool_timeout_seconds: Number(event.target.value),
                      })
                    }
                    required
                  />
                </label>
                <div className="button-row">
                  <button type="button" className="secondary" onClick={closeIntegrationAppDialog}>
                    {t("admin.cancel")}
                  </button>
                  <button type="submit" disabled={!systemSettingsLoaded}>
                    {selectedIntegrationApp ? t("admin.save") : t("admin.create")}
                  </button>
                </div>
                <fieldset className="form-section">
                  <legend>{t("admin.integrationAppTools")}</legend>
                  {selectedIntegrationApp ? (
                    <>
                      <p className="notice">{t("admin.integrationAppToolsManagedByApi")}</p>
                      <label className="readonly-field">
                        {t("admin.integrationAppToolsSyncUrl")}
                        <input readOnly value={integrationAppToolsSyncUrl} />
                      </label>
                      <p className="copy-line">{t("admin.integrationAppToolsSyncAuthHint")}</p>
                      <label className="readonly-field">
                        {t("admin.integrationAppToolsLastSyncedAt")}
                        <input
                          readOnly
                          value={
                            integrationToolsLastSyncedAt === null
                              ? t("admin.integrationAppToolsNotSynced")
                              : formatSchedulerSnapshotTime(integrationToolsLastSyncedAt, language)
                          }
                        />
                      </label>
                      <p className="copy-line">
                        {t("admin.integrationAppToolsCount", {
                          count: integrationTools.length,
                        })}
                      </p>
                      <div className="button-row">
                        <button
                          type="button"
                          className="secondary"
                          disabled={integrationToolsLoading}
                          onClick={() => void loadIntegrationAppTools(selectedIntegrationApp.id)}
                        >
                          <RotateCcw aria-hidden="true" size={16} />
                          {t("admin.refresh")}
                        </button>
                      </div>
                      {integrationToolsLoading ? (
                        <p className="notice">{t("admin.integrationAppToolsLoading")}</p>
                      ) : null}
                      {integrationTools.length === 0 ? (
                        <p className="notice">{t("admin.integrationAppToolsEmpty")}</p>
                      ) : (
                        <div className="table-scroll">
                          <table>
                            <thead>
                              <tr>
                                <th>{t("admin.integrationAppToolName")}</th>
                                <th>{t("admin.integrationAppToolDescription")}</th>
                                <th>{t("admin.integrationAppToolParameters")}</th>
                              </tr>
                            </thead>
                            <tbody>
                              {integrationTools.map((tool) => (
                                <tr key={tool.name}>
                                  <td>{tool.name}</td>
                                  <td>{tool.description}</td>
                                  <td>
                                    <pre className="integration-tool-parameters">
                                      {formatIntegrationToolParameters(tool.parameters)}
                                    </pre>
                                  </td>
                                </tr>
                              ))}
                            </tbody>
                          </table>
                        </div>
                      )}
                    </>
                  ) : (
                    <p className="notice">{t("admin.integrationAppToolsCreateHint")}</p>
                  )}
                </fieldset>
              </form>
            </div>
          </div>
        ) : null}
      </section>,
    );
  }

  if (activeTab === "skills") {
    const currentSkillPath = selectedSkillNode?.path ?? skillPathInput.trim();
    return renderSystemSettingsShell(
      <section className="admin-page" id="admin-skills">
        <div className="tab-actions">
          <div className="button-row">
            <button type="button" className="secondary" onClick={() => void refresh()}>
              {t("admin.refresh")}
            </button>
            <button type="button" className="secondary" onClick={newManagedSkill}>
              <FilePlus2 aria-hidden="true" size={16} />
              {t("admin.skillNew")}
            </button>
            <button type="button" className="secondary" onClick={newManagedSkillDirectory}>
              <FolderPlus aria-hidden="true" size={16} />
              {t("admin.skillNewFolder")}
            </button>
          </div>
        </div>
        {error ? <p className="error">{error}</p> : null}
        {skillSaved ? <p className="copy-line">{t("admin.skillSaved")}</p> : null}
        <div className="skills-layout">
          <div className="panel skills-list-panel">
            <div className="skill-upload-toolbar">
              <input
                ref={fileUploadInputRef}
                type="file"
                multiple
                hidden
                onChange={(event) => void uploadManagedSkillFiles(event)}
              />
              <input
                ref={folderUploadInputRef}
                type="file"
                multiple
                hidden
                data-testid="managed-skills-folder-input"
                // Chromium/WebKit 提供目录上传，React 类型暂未包含这个非标准属性。
                {...({ webkitdirectory: "", directory: "" } as Record<string, string>)}
                onChange={(event) => void uploadManagedSkillFiles(event)}
              />
              <button
                type="button"
                className="secondary"
                disabled={skillLoading}
                onClick={() => fileUploadInputRef.current?.click()}
              >
                <Upload aria-hidden="true" size={16} />
                {t("admin.skillUploadFiles")}
              </button>
              <button
                type="button"
                className="secondary"
                disabled={skillLoading}
                onClick={() => folderUploadInputRef.current?.click()}
              >
                <FolderPlus aria-hidden="true" size={16} />
                {t("admin.skillUploadFolder")}
              </button>
            </div>
            {!managedSkillTree || managedSkillTree.children.length === 0 ? (
              <p className="notice">{t("admin.skillEmpty")}</p>
            ) : (
              <ul className="list compact-list skill-list skill-tree">
                {managedSkillTree.children.map(renderManagedSkillNode)}
              </ul>
            )}
          </div>
          <form
            className="panel form skill-editor"
            onSubmit={(event) => void saveManagedSkill(event)}
          >
            {currentSkillPath ? (
              <p className="skill-path-line">
                {t("admin.skillPath")}
                {skillPathSeparator}
                {currentSkillPath}
              </p>
            ) : (
              <p className="notice">{t("admin.skillSelectOrCreate")}</p>
            )}
            {skillEditorMode === "file" ? (
              <MarkdownVditorEditor
                className="skill-vditor-editor"
                height={440}
                label={t("admin.skillContent")}
                value={skillContent}
                onChange={(nextContent) => {
                  setSkillContent(nextContent);
                  setSkillSaved(false);
                }}
              />
            ) : (
              <p className="notice">{t("admin.skillDirectorySelected")}</p>
            )}
            <div className="button-row">
              <button type="submit" disabled={skillLoading || currentSkillPath === ""}>
                {skillEditorMode === "directory" ? t("admin.skillCreateFolder") : t("admin.save")}
              </button>
              <button
                type="button"
                className="secondary"
                disabled={skillLoading || !selectedSkillNode}
                onClick={() => void deleteManagedSkill()}
              >
                {t("admin.delete")}
              </button>
            </div>
          </form>
        </div>
      </section>,
    );
  }

  return renderSystemSettingsShell(
    <section className="admin-page" id="admin-users">
      <div className="tab-actions">
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
                  {invite.used_count}/{invite.max_uses} {t("admin.used")} · {t("admin.expiresAt")}{" "}
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
    </section>,
  );
}
