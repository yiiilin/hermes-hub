import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";

import type { ExampleAuthFlow } from "./auth-flow";
import { createExampleAuthFlow } from "./auth-flow";
import { createChatRuntime, type ExampleToolCallState } from "./chat-runtime";
import {
  createHubClient,
  type ExampleHubClient,
  type ExampleHubConfig,
  type ExampleMessage,
  type ExampleSessionEvent,
  type ExampleSessionSummary,
  type ExampleUserInfo,
} from "./hub-client";
import { createIndexedDbNoteStore, type ExampleNote, type ExampleNoteStore } from "./indexeddb-store";
import { isBusinessToolRequestProtocolMessage } from "./protocol";
import {
  clearStoredAccessToken,
  clearStoredPendingToolResults,
  clearStoredPendingToolResultsForSession,
  clearStoredUserInfo,
  EXAMPLE_STORAGE_KEYS,
  isStoredAccessTokenExpired,
  loadStoredAccessToken,
  loadStoredConfig,
  loadStoredSelectedSessionId,
  loadStoredUserInfo,
  saveStoredConfig,
  saveStoredSelectedSessionId,
  type StoredAccessToken,
} from "./storage";
import { createLocalToolRegistry } from "./tool-registry";

type HermesHubExampleDependencies = {
  client: ExampleHubClient;
  noteStore: ExampleNoteStore;
  authFlow: ExampleAuthFlow;
  initialConfig: ExampleHubConfig;
};

type HermesHubExampleAppProps = {
  dependencies?: Partial<HermesHubExampleDependencies>;
};

type StatusTone = "idle" | "ready" | "running" | "success" | "error";

type StatusState = {
  tone: StatusTone;
  message: string;
};

const INITIAL_TOOL_STATUS: StatusState = {
  tone: "idle",
  message: "尚未同步工具",
};

function createWaitingToolStatus(message = "等待下一次工具调用"): StatusState {
  return {
    tone: "idle",
    message,
  };
}

export function HermesHubExampleApp(props: HermesHubExampleAppProps) {
  const initialConfig = props.dependencies?.initialConfig ?? loadConfigWithFallback();
  const client = useMemo(
    () => props.dependencies?.client ?? createHubClient(),
    [props.dependencies?.client],
  );
  const noteStore = useMemo(
    () => props.dependencies?.noteStore ?? createIndexedDbNoteStore(),
    [props.dependencies?.noteStore],
  );
  const authFlow = useMemo(
    () => props.dependencies?.authFlow ?? createExampleAuthFlow(client),
    [client, props.dependencies?.authFlow],
  );
  const toolRegistry = useMemo(() => createLocalToolRegistry(noteStore), [noteStore]);

  const [config, setConfig] = useState<ExampleHubConfig>(initialConfig);
  const [token, setToken] = useState<StoredAccessToken | null>(() => loadStoredAccessToken());
  const [userInfo, setUserInfo] = useState<ExampleUserInfo | null>(() => loadStoredUserInfo());
  const [sessions, setSessions] = useState<ExampleSessionSummary[]>([]);
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(
    () => loadStoredSelectedSessionId(),
  );
  const [messages, setMessages] = useState<ExampleMessage[]>([]);
  const [toolCalls, setToolCalls] = useState<ExampleToolCallState[]>([]);
  const [eventLogs, setEventLogs] = useState<string[]>([]);
  const [localNotes, setLocalNotes] = useState<ExampleNote[]>([]);
  const [composerValue, setComposerValue] = useState("");
  const [hubStatus, setHubStatus] = useState<StatusState>({
    tone: hasConnectionConfig(initialConfig) ? "ready" : "idle",
    message: hasConnectionConfig(initialConfig) ? "连接配置已载入" : "等待填写 Hub 配置",
  });
  const [loginStatus, setLoginStatus] = useState<StatusState>({
    tone: "idle",
    message: "尚未登录",
  });
  const [toolStatus, setToolStatus] = useState<StatusState>({
    ...INITIAL_TOOL_STATUS,
  });
  const [isSending, setIsSending] = useState(false);
  const [isAuthRestoring, setIsAuthRestoring] = useState(true);
  const runtimeStopRef = useRef<(() => void) | null>(null);

  function appendLog(message: string) {
    setEventLogs((previous) => [appendTimestampPrefix(message), ...previous].slice(0, 60));
  }

  function resetToolActivity(message?: string) {
    setToolCalls([]);
    setToolStatus(createWaitingToolStatus(message));
  }

  useEffect(() => {
    void restoreAuthState();
    void refreshLocalNotes();
    // 首屏只需要执行一次状态恢复。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    saveStoredSelectedSessionId(selectedSessionId);
  }, [selectedSessionId]);

  useEffect(() => {
    runtimeStopRef.current?.();
    runtimeStopRef.current = null;

    if (isAuthRestoring || !token?.accessToken || !selectedSessionId) {
      return;
    }

    setHubStatus({
      tone: "ready",
      message: "Hub 接入链路可用，正在监听会话事件",
    });

    const runtime = createChatRuntime({
      client,
      config,
      accessToken: token.accessToken,
      sessionId: selectedSessionId,
      toolRegistry,
      onToolCallUpdate(toolCall) {
        setToolCalls((previous) => mergeToolCalls(previous, toolCall));
        // 工具状态需要给页面一个明确总览，不要求用户盯着日志看。
        setToolStatus({
          tone:
            toolCall.status === "completed"
              ? "success"
              : toolCall.status === "running"
                ? "running"
                : "error",
          message: `${toolCall.toolName}: ${toolCall.summary}`,
        });
      },
      onSessionEvent(event) {
        applySessionEvent(event);
      },
      onEventLog(entry) {
        appendLog(entry);
      },
      onLocalToolSuccess() {
        void refreshLocalNotes();
      },
      onStreamDisconnect(error) {
        setHubStatus({
          tone: "error",
          message: `会话事件流已断开：${error.message}`,
        });
      },
    });
    runtime.start();
    runtimeStopRef.current = runtime.stop;

    return () => {
      runtime.stop();
      runtimeStopRef.current = null;
    };
  }, [client, config, isAuthRestoring, selectedSessionId, token?.accessToken, toolRegistry]);

  async function restoreAuthState() {
    const callbackUrl = new URL(window.location.href);
    const wasHandlingOAuthCallback =
      callbackUrl.searchParams.has("code") || callbackUrl.searchParams.has("error");
    try {
      const restored = await authFlow.completeAuthorizationFromLocation(config);
      if (restored) {
        setToken(restored.accessToken);
        setUserInfo(restored.userInfo);
        setLoginStatus({
          tone: "ready",
          message: restored.userInfo.email,
        });
        await refreshSessions(restored.accessToken.accessToken);
        appendLog("OAuth 回调完成，已恢复 bearer token");
        return;
      }

      if (token?.accessToken && isStoredAccessTokenExpired(token)) {
        clearStoredAccessToken();
        clearStoredPendingToolResults();
        clearStoredUserInfo();
        setToken(null);
        setUserInfo(null);
        setLoginStatus({
          tone: "idle",
          message: "本地 token 已过期，请重新登录",
        });
        appendLog("检测到本地 token 已过期，已清除");
        return;
      }

      if (token?.accessToken && !userInfo) {
        clearStoredAccessToken();
        clearStoredPendingToolResults();
        setToken(null);
        setLoginStatus({
          tone: "idle",
          message: "本地登录信息不完整，请重新登录",
        });
        appendLog("本地只有 token 没有 userinfo，已清除");
        return;
      }

      if (token?.accessToken && userInfo) {
        setLoginStatus({
          tone: "ready",
          message: userInfo.email,
        });
        await refreshSessions(token.accessToken);
        appendLog("已从本地存储恢复登录状态");
      }
    } catch (error) {
      if (wasHandlingOAuthCallback) {
        clearStoredAccessToken();
        clearStoredPendingToolResults();
        clearStoredUserInfo();
        setToken(null);
        setUserInfo(null);
        setSessions([]);
        setSelectedSessionId(null);
        setMessages([]);
        resetToolActivity("等待重新登录后再执行工具");
      }
      const message = error instanceof Error ? error.message : "登录恢复失败";
      setLoginStatus({
        tone: "error",
        message,
      });
      appendLog(`OAuth 回调失败：${message}`);
    } finally {
      setIsAuthRestoring(false);
    }
  }

  async function refreshSessions(accessToken: string) {
    const nextSessions = await client.listSessions(config, accessToken);
    setSessions(nextSessions);

    const remembered = loadStoredSelectedSessionId();
    const nextSelectedSession =
      nextSessions.find((session) => session.id === selectedSessionId) ??
      nextSessions.find((session) => session.id === remembered) ??
      nextSessions[0] ??
      null;

    if (nextSelectedSession?.id !== selectedSessionId) {
      setMessages([]);
      resetToolActivity();
    }
    setSelectedSessionId(nextSelectedSession?.id ?? null);
    if (!nextSelectedSession) {
      setMessages([]);
      resetToolActivity();
    }
  }

  async function refreshLocalNotes() {
    try {
      setLocalNotes(await noteStore.listNotes());
    } catch (error) {
      appendLog(`读取 IndexedDB 失败：${error instanceof Error ? error.message : "未知错误"}`);
    }
  }

  function applySessionEvent(event: ExampleSessionEvent) {
    switch (event.type) {
      case "messages_snapshot":
        setMessages(filterVisibleMessages(event.messages));
        if (event.session) {
          const nextSession = event.session;
          setSessions((previous) => mergeSessions(previous, nextSession));
        }
        void refreshLocalNotes();
        break;
      case "message_created":
      case "message_updated":
        if (!isBusinessToolRequestProtocolMessage(event.message)) {
          setMessages((previous) => mergeMessages(previous, event.message));
        }
        break;
      case "session_updated":
        setSessions((previous) => mergeSessions(previous, event.session));
        break;
      case "session_deleted":
        clearStoredPendingToolResultsForSession(event.sessionId);
        setSessions((previous) => previous.filter((session) => session.id !== event.sessionId));
        if (selectedSessionId === event.sessionId) {
          setSelectedSessionId(null);
          setMessages([]);
          resetToolActivity();
        }
        break;
      case "run_updated":
        appendLog(`Run 状态更新：${event.run.status}`);
        break;
      case "run_cleared":
        appendLog(`Run 已清空：${event.sessionId}`);
        break;
      case "business_tool_request":
        break;
    }
  }

  function handleConfigChange<Key extends keyof ExampleHubConfig>(
    key: Key,
    value: ExampleHubConfig[Key],
  ) {
    setConfig((previous) => ({
      ...previous,
      [key]: value,
    }));
  }

  function handleSaveConfig() {
    saveStoredConfig(config);
    setHubStatus({
      tone: hasConnectionConfig(config) ? "ready" : "idle",
      message: hasConnectionConfig(config) ? "配置已保存" : "配置还不完整",
    });
    appendLog(`已保存配置到 ${EXAMPLE_STORAGE_KEYS.config}`);
  }

  function handleSelectSession(sessionId: string) {
    if (sessionId === selectedSessionId) {
      return;
    }
    setSelectedSessionId(sessionId);
    // 会话切换后先清掉旧 UI，避免在新 snapshot 返回前误显示上一个 session 的内容。
    setMessages([]);
    resetToolActivity();
    appendLog(`切换到会话：${sessionId}`);
  }

  async function handleSyncTools() {
    try {
      setToolStatus({
        tone: "running",
        message: "正在同步 save_note / search_notes",
      });
      await client.replaceTools(config, toolRegistry.definitions);
      setToolStatus({
        tone: "success",
        message: "工具已同步",
      });
      setHubStatus({
        tone: "ready",
        message: "Hub 接入链路可用",
      });
      appendLog("已同步本地工具定义");
    } catch (error) {
      const message = error instanceof Error ? error.message : "工具同步失败";
      setToolStatus({
        tone: "error",
        message,
      });
      setHubStatus({
        tone: "error",
        message,
      });
      appendLog(`工具同步失败：${message}`);
    }
  }

  function handleBeginLogin() {
    try {
      // OAuth 回调回来后需要依赖本地保存的配置继续换 token，所以登录前总是先持久化当前表单值。
      saveStoredConfig(config);
      authFlow.beginAuthorization(config);
    } catch (error) {
      const message = error instanceof Error ? error.message : "发起登录失败";
      setLoginStatus({
        tone: "error",
        message,
      });
      appendLog(`发起登录失败：${message}`);
    }
  }

  function handleClearLogin() {
    clearStoredAccessToken();
    clearStoredPendingToolResults();
    clearStoredUserInfo();
    setToken(null);
    setUserInfo(null);
    setSessions([]);
    setSelectedSessionId(null);
    setMessages([]);
    resetToolActivity("等待登录后再执行工具");
    setLoginStatus({
      tone: "idle",
      message: "本地登录状态已清除",
    });
    appendLog("已清除本地 OAuth token");
  }

  async function handleCreateSession() {
    if (!token?.accessToken) {
      return;
    }

    try {
      const session = await client.createSession(config, token.accessToken, {
        kind: "agent",
        title: "IndexedDB 笔记演示",
      });
      setSessions((previous) => [session, ...previous]);
      setSelectedSessionId(session.id);
      setMessages([]);
      resetToolActivity();
      appendLog(`已创建示例会话：${session.id}`);
    } catch (error) {
      appendLog(`创建会话失败：${error instanceof Error ? error.message : "未知错误"}`);
    }
  }

  async function handleDeleteSession(sessionId: string) {
    if (!token?.accessToken) {
      return;
    }

    try {
      await client.deleteSession(config, token.accessToken, sessionId);
      clearStoredPendingToolResultsForSession(sessionId);
      setSessions((previous) => previous.filter((session) => session.id !== sessionId));
      if (selectedSessionId === sessionId) {
        setSelectedSessionId(null);
        setMessages([]);
        resetToolActivity();
      }
      appendLog(`已删除会话：${sessionId}`);
    } catch (error) {
      appendLog(`删除会话失败：${error instanceof Error ? error.message : "未知错误"}`);
    }
  }

  async function handleSendMessage() {
    const content = composerValue.trim();
    if (!content || !token?.accessToken || !selectedSessionId) {
      return;
    }

    try {
      setIsSending(true);
      const message = await client.appendMessage(config, token.accessToken, selectedSessionId, {
        role: "user",
        content,
      });
      setMessages((previous) => mergeMessages(previous, message));
      setComposerValue("");
      appendLog("已发送用户消息");
    } catch (error) {
      appendLog(`发送消息失败：${error instanceof Error ? error.message : "未知错误"}`);
    } finally {
      setIsSending(false);
    }
  }

  return (
    <div className="example-shell">
      <header className="hero">
        <div className="hero-copy">
          <p className="eyebrow">Frontend Example</p>
          <h1>Hermes-Hub 纯前端接入示例</h1>
          <p>
            浏览器内直接完成工具注册、OAuth 授权、session 事件订阅和本地 IndexedDB
            工具执行。这个页面故意不解决生产安全问题，只服务于演示和联调。
          </p>
        </div>
        <div className="status-grid">
          <StatusCard title="Hub 连接状态" status={hubStatus} />
          <StatusCard title="OAuth 登录状态" status={loginStatus} />
          <StatusCard title="工具调用状态" status={toolStatus} />
        </div>
      </header>

      <main className="content">
        <section className="panel">
          <div className="panel-header">
            <div>
              <h2>连接配置</h2>
              <p>管理员需预先创建 integration app；示例仅消费现有 client_id / client_secret。</p>
            </div>
            <div className="action-row">
              <button type="button" onClick={handleSaveConfig}>
                保存配置
              </button>
              <button type="button" onClick={handleSyncTools}>
                同步工具
              </button>
            </div>
          </div>
          <div className="form-grid">
            <Field label="Hermes-Hub Base URL">
              <input
                value={config.baseUrl}
                onChange={(event) => handleConfigChange("baseUrl", event.target.value)}
              />
            </Field>
            <Field label="Client ID">
              <input
                value={config.clientId}
                onChange={(event) => handleConfigChange("clientId", event.target.value)}
              />
            </Field>
            <Field label="Client Secret">
              <input
                type="password"
                value={config.clientSecret}
                onChange={(event) => handleConfigChange("clientSecret", event.target.value)}
              />
            </Field>
            <Field label="Redirect URI">
              <input
                value={config.redirectUri}
                onChange={(event) => handleConfigChange("redirectUri", event.target.value)}
              />
            </Field>
            <Field label="Scopes">
              <input
                value={config.scopes}
                onChange={(event) => handleConfigChange("scopes", event.target.value)}
              />
            </Field>
          </div>
        </section>

        <section className="layout-grid">
          <aside className="panel sidebar">
            <div className="panel-header">
              <div>
                <h2>用户与会话</h2>
                <p>{userInfo?.email ?? "未登录"}</p>
              </div>
              <div className="action-row">
                <button type="button" onClick={handleBeginLogin}>
                  登录到 Hermes-Hub
                </button>
                <button type="button" className="ghost-button" onClick={handleClearLogin}>
                  清除本地登录
                </button>
              </div>
            </div>

            <div className="panel-subsection">
              <div className="subsection-header">
                <h3>会话列表</h3>
                <button type="button" onClick={handleCreateSession} disabled={!token?.accessToken}>
                  新建会话
                </button>
              </div>
              <div className="session-list">
                {sessions.length === 0 ? (
                  <div className="empty-state">当前没有可用 session。</div>
                ) : (
                  sessions.map((session) => (
                    <button
                      key={session.id}
                      type="button"
                      className={`session-card ${session.id === selectedSessionId ? "selected" : ""}`}
                      onClick={() => handleSelectSession(session.id)}
                    >
                      <strong>{session.title || session.id}</strong>
                      <small>{session.id}</small>
                      {session.deletable ? (
                        <span
                          role="button"
                          tabIndex={0}
                          className="inline-link"
                          onClick={(event) => {
                            event.stopPropagation();
                            void handleDeleteSession(session.id);
                          }}
                          onKeyDown={(event) => {
                            if (event.key === "Enter" || event.key === " ") {
                              event.preventDefault();
                              void handleDeleteSession(session.id);
                            }
                          }}
                        >
                          删除
                        </span>
                      ) : null}
                    </button>
                  ))
                )}
              </div>
            </div>

            <div className="panel-subsection">
              <div className="subsection-header">
                <h3>本地笔记库</h3>
                <span>{localNotes.length} 条</span>
              </div>
              <div className="note-list">
                {localNotes.length === 0 ? (
                  <div className="empty-state">IndexedDB 里还没有笔记。</div>
                ) : (
                  localNotes.slice(0, 6).map((note) => (
                    <article key={note.id} className="note-card">
                      <strong>{note.title || "未命名笔记"}</strong>
                      <p>{note.content}</p>
                      <small>{note.tags.join(", ") || "无标签"}</small>
                    </article>
                  ))
                )}
              </div>
            </div>
          </aside>

          <section className="panel chat-panel">
            <div className="panel-header">
              <div>
                <h2>对话区</h2>
                <p>{selectedSessionId ?? "先选择一个会话"}</p>
              </div>
            </div>

            <div className="chat-layout">
              <div className="chat-column">
                <div className="chat-list">
                  {messages.length === 0 ? (
                    <div className="empty-state">
                      发送消息后，assistant 可以通过 `save_note` / `search_notes`
                      请求浏览器本地工具。
                    </div>
                  ) : (
                    messages.map((message) => (
                      <article key={message.id} className={`message-bubble ${message.role}`}>
                        <header>
                          <strong>{message.role === "user" ? "User" : "Assistant"}</strong>
                          <span>{formatTime(message.createdAt)}</span>
                        </header>
                        <p>{message.content}</p>
                      </article>
                    ))
                  )}
                </div>

                <div className="composer">
                  <label htmlFor="example-message-input">输入消息</label>
                  <textarea
                    id="example-message-input"
                    value={composerValue}
                    onChange={(event) => setComposerValue(event.target.value)}
                    placeholder="例如：请帮我保存一条笔记，再检索一下刚才存过的内容。"
                  />
                  <div className="action-row">
                    <button
                      type="button"
                      onClick={handleSendMessage}
                      disabled={!selectedSessionId || !token?.accessToken || isSending}
                    >
                      发送
                    </button>
                  </div>
                </div>
              </div>

              <div className="side-column">
                <section className="tool-panel">
                  <h3>工具调用</h3>
                  <div className="tool-call-list">
                    {toolCalls.length === 0 ? (
                      <div className="empty-state">还没有工具调用。</div>
                    ) : (
                      toolCalls.map((toolCall) => (
                        <article key={toolCall.requestId} className={`tool-call ${toolCall.status}`}>
                          <header>
                            <strong>{toolCall.toolName}</strong>
                            <span>{toolCall.status}</span>
                          </header>
                          <p>{toolCall.summary}</p>
                          <pre>{toolCall.argumentsPreview}</pre>
                          {toolCall.resultPreview ? <pre>{toolCall.resultPreview}</pre> : null}
                        </article>
                      ))
                    )}
                  </div>
                </section>

                <section className="log-panel">
                  <h3>事件日志</h3>
                  <div className="log-list">
                    {eventLogs.length === 0 ? (
                      <div className="empty-state">还没有日志。</div>
                    ) : (
                      eventLogs.map((entry) => <div key={entry}>{entry}</div>)
                    )}
                  </div>
                </section>
              </div>
            </div>
          </section>
        </section>
      </main>
    </div>
  );
}

function Field(props: { label: string; children: ReactNode }) {
  return (
    <label className="field">
      <span>{props.label}</span>
      {props.children}
    </label>
  );
}

function StatusCard(props: { title: string; status: StatusState }) {
  return (
    <article className={`status-card ${props.status.tone}`}>
      <span>{props.title}</span>
      <strong>{props.status.message}</strong>
    </article>
  );
}

function filterVisibleMessages(messages: ExampleMessage[]): ExampleMessage[] {
  return messages.filter((message) => !isBusinessToolRequestProtocolMessage(message));
}

function mergeMessages(currentMessages: ExampleMessage[], nextMessage: ExampleMessage): ExampleMessage[] {
  const map = new Map(currentMessages.map((message) => [message.id, message]));
  map.set(nextMessage.id, nextMessage);
  return Array.from(map.values()).sort((left, right) => left.createdAt - right.createdAt);
}

function mergeSessions(
  currentSessions: ExampleSessionSummary[],
  nextSession: ExampleSessionSummary,
): ExampleSessionSummary[] {
  const map = new Map(currentSessions.map((session) => [session.id, session]));
  map.set(nextSession.id, nextSession);
  return Array.from(map.values()).sort((left, right) => right.updatedAt - left.updatedAt);
}

function mergeToolCalls(
  currentCalls: ExampleToolCallState[],
  nextCall: ExampleToolCallState,
): ExampleToolCallState[] {
  const map = new Map(currentCalls.map((toolCall) => [toolCall.requestId, toolCall]));
  map.set(nextCall.requestId, nextCall);
  return Array.from(map.values());
}

function appendTimestampPrefix(message: string): string {
  const time = new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).format(new Date());
  return `${time} ${message}`;
}

function hasConnectionConfig(config: ExampleHubConfig): boolean {
  return Boolean(
    config.baseUrl.trim() &&
      config.clientId.trim() &&
      config.clientSecret.trim() &&
      config.redirectUri.trim(),
  );
}

function formatTime(timestamp: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(timestamp));
}

function loadConfigWithFallback(): ExampleHubConfig {
  const storedConfig = loadStoredConfig();
  if (storedConfig) {
    return storedConfig;
  }

  const currentUrl = new URL(window.location.href);
  return {
    baseUrl: currentUrl.origin,
    clientId: "",
    clientSecret: "",
    redirectUri: `${currentUrl.origin}${currentUrl.pathname}`,
    scopes: "openid profile email",
  };
}
