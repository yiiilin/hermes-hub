import type {
  ApiClient,
  ChannelMessage,
  ChannelRun,
  ChannelSessionEvent,
  HermesActiveRun,
  HermesAttachment,
  HermesVerboseEvent,
  SessionSummary,
} from "../api/client";
import { ApiRequestError } from "../api/client";
import { useChatSidebar, useSidebarCollapsed } from "../components/layout";
import { useI18n } from "../i18n";
import {
  File,
  FileArchive,
  FileAudio,
  FileCode2,
  FileImage,
  FileSpreadsheet,
  Bot,
  CircleStop,
  FileText,
  FileType,
  Image,
  MessageSquare,
  Paperclip,
  Plus,
  Presentation,
  RefreshCw,
  Send,
  Trash2,
  X,
  FileVideo,
} from "lucide-react";
import { FormEvent, ReactNode, useEffect, useRef, useState } from "react";

type ChannelSessionRouteProps = {
  active?: boolean;
  apiClient: ApiClient;
  onOpenChat?: () => void;
};

type BrowserCrypto = {
  randomUUID?: () => string;
  getRandomValues?: <T extends Uint8Array>(array: T) => T;
};

type Translate = ReturnType<typeof useI18n>["t"];
type ExecutionHistoryEntry = HermesVerboseEvent;

export function ChannelSessionRoute({
  active = true,
  apiClient,
  onOpenChat,
}: ChannelSessionRouteProps) {
  const { t } = useI18n();
  const setChatSidebar = useChatSidebar();
  const sidebarCollapsed = useSidebarCollapsed();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [selectedSession, setSelectedSession] = useState<SessionSummary | null>(null);
  const [seenSessionUpdates, setSeenSessionUpdates] = useState<Record<string, number>>({});
  const [unreadSessionIds, setUnreadSessionIds] = useState<Set<string>>(() => new Set());
  const [messages, setMessages] = useState<ChannelMessage[]>([]);
  const [prompt, setPrompt] = useState("");
  const [attachments, setAttachments] = useState<HermesAttachment[]>([]);
  const [previewAttachment, setPreviewAttachment] = useState<HermesAttachment | null>(null);
  const [pendingAssistantMessageId, setPendingAssistantMessageId] = useState<string | null>(null);
  const [pendingAssistantSessionId, setPendingAssistantSessionId] = useState<string | null>(null);
  const [activeRun, setActiveRun] = useState<HermesActiveRun | null>(null);
  const [verboseEvents, setVerboseEvents] = useState<ExecutionHistoryEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);
  const messageListRef = useRef<HTMLDivElement | null>(null);
  const stickToBottomRef = useRef(true);
  const pendingAssistantIdsByRunRef = useRef<Record<string, string>>({});
  const verboseEventsRef = useRef<Record<string, ExecutionHistoryEntry[]>>({});
  const executionPersistQueueRef = useRef<Record<string, Promise<ChannelMessage | null>>>({});
  const selectedSessionIdRef = useRef<string | null>(null);
  const activeRunRef = useRef<HermesActiveRun | null>(null);
  const messagesRef = useRef<ChannelMessage[]>([]);
  const pendingAssistantMessageIdRef = useRef<string | null>(null);
  const pendingAssistantSessionIdRef = useRef<string | null>(null);

  useEffect(() => {
    selectedSessionIdRef.current = selectedSession?.id ?? null;
  }, [selectedSession?.id]);

  useEffect(() => {
    activeRunRef.current = activeRun;
  }, [activeRun]);

  useEffect(() => {
    messagesRef.current = messages;
  }, [messages]);

  function markPendingAssistantMessage(sessionId: string, messageId: string) {
    pendingAssistantMessageIdRef.current = messageId;
    pendingAssistantSessionIdRef.current = sessionId;
    if (selectedSessionIdRef.current === sessionId) {
      setPendingAssistantMessageId(messageId);
      setPendingAssistantSessionId(sessionId);
    }
  }

  function clearPendingAssistantMessage() {
    pendingAssistantMessageIdRef.current = null;
    pendingAssistantSessionIdRef.current = null;
    setPendingAssistantMessageId(null);
    setPendingAssistantSessionId(null);
  }

  function focusComposerInputSoon() {
    const schedule = globalThis.requestAnimationFrame ?? ((callback: FrameRequestCallback) => {
      globalThis.setTimeout(callback, 0);
      return 0;
    });
    schedule(() => composerInputRef.current?.focus());
  }

  async function refreshSessions() {
    setError(null);
    try {
      const nextSessions = await apiClient.listSessionsPublic();
      const selectedSessionId = selectedSession?.id;
      const nextSessionIds = new Set(nextSessions.map((session) => session.id));
      setSessions(nextSessions);
      setUnreadSessionIds((current) => {
        const next = new Set(current);
        for (const session of nextSessions) {
          const lastSeen = seenSessionUpdates[session.id];
          if (
            lastSeen !== undefined &&
            sessionUpdatedAt(session) > lastSeen &&
            session.id !== selectedSessionId
          ) {
            next.add(session.id);
          }
        }
        for (const sessionId of next) {
          if (!nextSessionIds.has(sessionId) || sessionId === selectedSessionId) {
            next.delete(sessionId);
          }
        }
        return next;
      });

      setSeenSessionUpdates((current) => {
        const next = { ...current };
        for (const session of nextSessions) {
          if (next[session.id] === undefined || session.id === selectedSessionId) {
            next[session.id] = sessionUpdatedAt(session);
          }
        }
        return next;
      });

      const nextSelected = selectedSessionId
        ? nextSessions.find((session) => session.id === selectedSessionId) ?? null
        : nextSessions[0] ?? null;
      selectedSessionIdRef.current = nextSelected?.id ?? null;
      setSelectedSession(nextSelected);
      if (!nextSelected) {
        setMessages([]);
        setActiveRun(null);
        clearPendingAssistantMessage();
        resetVerboseEvents();
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.workspaceLoadFailed"));
    }
  }

  useEffect(() => {
    void refreshSessions();
  }, []);

  useEffect(() => {
    if (!active || !selectedSession) {
      return;
    }

    const session = selectedSession;
    // 浏览器只订阅当前激活会话的 room；断线后 EventSource 自动重连，重连首包会重新同步历史。
    return apiClient.subscribeSessionEventsPublic(
      session.id,
      (event) => {
        void handleSessionEvent(session, event);
      },
      () => {
        // EventSource 会自动重连。这里不设置页面错误，避免切后台/刷新造成 load failed 假错误。
      },
    );
  }, [active, apiClient, selectedSession?.id]);

  useEffect(() => {
    const node = messageListRef.current;
    if (!node) {
      return;
    }

    function handleScroll() {
      const current = messageListRef.current;
      if (current) {
        stickToBottomRef.current = isMessageListNearBottom(current);
      }
    }

    handleScroll();
    node.addEventListener("scroll", handleScroll, { passive: true });
    return () => node.removeEventListener("scroll", handleScroll);
  }, [selectedSession?.id]);

  useEffect(() => {
    if (stickToBottomRef.current) {
      scrollMessageListToBottom(messageListRef.current);
    }
  }, [messages, verboseEvents]);

  useEffect(() => {
    stickToBottomRef.current = true;
    scrollMessageListToBottom(messageListRef.current);
  }, [selectedSession?.id]);

  useEffect(() => {
    setChatSidebar?.(
      <ChatSidebar
        sessions={sessions}
        selectedSession={selectedSession}
        collapsed={sidebarCollapsed}
        unreadSessionIds={unreadSessionIds}
        onCreate={() => void createSidebarSession()}
        onSelect={(session) => void selectSidebarSession(session)}
        onDelete={(session) => void deleteSidebarSession(session)}
      />,
    );

    return () => setChatSidebar?.(null);
  }, [sessions, selectedSession, setChatSidebar, sidebarCollapsed, t, unreadSessionIds]);

  async function createSession() {
    const session = await apiClient.createSessionPublic("agent");
    setSessions((current) => [session, ...current]);
    selectedSessionIdRef.current = session.id;
    setSelectedSession(session);
    setSeenSessionUpdates((current) => ({
      ...current,
      [session.id]: sessionUpdatedAt(session),
    }));
    setUnreadSessionIds((current) => withoutSession(current, session.id));
    setMessages([]);
    setActiveRun(null);
    clearPendingAssistantMessage();
    resetVerboseEvents();
    clearExecutionHistory(session.id);
    stickToBottomRef.current = true;
    return session;
  }

  async function createSidebarSession() {
    onOpenChat?.();
    try {
      setError(null);
      const session = await createSession();
      if (!session) {
        throw new Error(t("chat.sessionCreateFailed"));
      }
    } catch (cause) {
      setError(userFacingErrorMessage(cause, t("chat.sessionCreateFailed"), t));
    }
  }

  async function selectSession(session: SessionSummary) {
    if (selectedSessionIdRef.current === session.id) {
      setSeenSessionUpdates((current) => ({
        ...current,
        [session.id]: sessionUpdatedAt(session),
      }));
      setUnreadSessionIds((current) => withoutSession(current, session.id));
      return;
    }

    selectedSessionIdRef.current = session.id;
    setSelectedSession(session);
    clearPendingAssistantMessage();
    setActiveRun(null);
    resetVerboseEvents();
    setSeenSessionUpdates((current) => ({
      ...current,
      [session.id]: sessionUpdatedAt(session),
    }));
    setUnreadSessionIds((current) => withoutSession(current, session.id));
    stickToBottomRef.current = true;
    setMessages([]);
  }

  async function selectSidebarSession(session: SessionSummary) {
    onOpenChat?.();
    await selectSession(session);
  }

  async function restoreActiveRun(
    session: SessionSummary,
    currentMessages: ChannelMessage[],
    knownRun?: HermesActiveRun | null,
  ) {
    const run = knownRun ?? null;
    if (selectedSessionIdRef.current !== session.id) {
      return;
    }
    if (!run) {
      setActiveRun(null);
      clearPendingAssistantMessage();
      resetVerboseEvents();
      return;
    }
    if (!isHubRunId(run.run_id)) {
      // 只接受 Hub 规范的 run id；其他来源的状态不进入聊天态。
      setActiveRun(null);
      clearPendingAssistantMessage();
      resetVerboseEvents();
      return;
    }

    setActiveRun(run);
    if (isTerminalHermesRun(run)) {
      await persistTerminalRun(session.id, run, currentMessages);
      clearPendingAssistantMessage();
      resetVerboseEvents();
      return;
    }

    resumeAdapterRun(session.id, run);
  }

  function resumeAdapterRun(sessionId: string, run: HermesActiveRun) {
    const attachedPendingId = pendingAssistantIdsByRunRef.current[run.run_id];
    const assistantMessageId = ensurePendingAssistantPlaceholder(sessionId, run, attachedPendingId);
    pendingAssistantIdsByRunRef.current[run.run_id] = assistantMessageId;
  }

  async function handleSessionEvent(
    session: SessionSummary,
    event: ChannelSessionEvent,
  ) {
    if (selectedSessionIdRef.current !== session.id) {
      return;
    }

    if (event.type === "messages_snapshot") {
      const nextMessages = sortMessagesForDisplay(event.messages);
      setMessages(nextMessages);
      hydrateExecutionHistory(session.id, nextMessages, event.active_run);
      await restoreActiveRun(session, nextMessages, event.active_run);
      return;
    }

    if (event.type === "message_created" || event.type === "message_updated") {
      const message = event.message;
      const runId = runIdFromHermesMessageKey(message.client_message_key);
      const pendingId = runId ? pendingAssistantIdsByRunRef.current[runId] : undefined;
      updateMessagesForSession(session.id, (current) => {
        const withoutPending =
          message.role === "assistant" && hasRenderableMessageBody(message)
            ? removePendingAssistantForMessage(current, message, pendingId)
            : current;
        return mergeMessagesById(withoutPending, [message]);
      });
      if (message.role === "assistant") {
        const run = activeRunRef.current;
        const activeRunId = run?.run_id ?? runId;
        const runStillActive = Boolean(activeRunId && (!run || !isTerminalHermesRun(run)));
        const hasPendingMessageInSession = Boolean(
          pendingAssistantMessageIdRef.current &&
            pendingAssistantSessionIdRef.current === message.session_id,
        );
        if ((activeRunId && runStillActive) || hasPendingMessageInSession) {
          if (isExecutionHistoryContent(message.content)) {
            // 执行日志本身就是当前可见的 loading 气泡，避免再保留一个空回复气泡。
            if (activeRunId) {
              pendingAssistantIdsByRunRef.current[activeRunId] = message.id;
            }
            markPendingAssistantMessage(message.session_id, message.id);
          } else if (
            activeRunId &&
            message.client_message_key === hermesRunMessageKey(activeRunId) &&
            hasRenderableMessageBody(message)
          ) {
            // 正式回复开始流式出现后，把 loading 挂到真实回复气泡上，直到 run 结束。
            pendingAssistantIdsByRunRef.current[activeRunId] = message.id;
            markPendingAssistantMessage(message.session_id, message.id);
            if (selectedSessionIdRef.current === message.session_id) {
              resetVerboseEvents();
            }
          }
        }
      }
      return;
    }

    if (event.type === "run_updated") {
      const run = activeRunFromChannelRun(event.run);
      setActiveRun(run);
      if (isTerminalHermesRun(run)) {
        await persistTerminalRun(session.id, run, messagesRef.current);
        delete pendingAssistantIdsByRunRef.current[run.run_id];
        clearPendingAssistantMessage();
        clearExecutionHistory(session.id);
      } else {
        resumeAdapterRun(session.id, run);
      }
      return;
    }

    if (event.type === "run_cleared") {
      if (activeRunRef.current) {
        delete pendingAssistantIdsByRunRef.current[activeRunRef.current.run_id];
      }
      setActiveRun(null);
      clearPendingAssistantMessage();
      clearExecutionHistory(session.id);
      return;
    }

    if (event.type === "session_deleted") {
      await refreshSessions();
    }
  }

  function ensurePendingAssistantPlaceholder(
    sessionId: string,
    run: HermesActiveRun,
    preferredMessageId?: string,
  ) {
    const assistantMessageId = preferredMessageId ?? `pending-${run.run_id}`;
    markPendingAssistantMessage(sessionId, assistantMessageId);
    updateMessagesForSession(sessionId, (current) =>
      current.some((message) => message.id === assistantMessageId)
        ? current
        : [
            ...current,
            {
              id: assistantMessageId,
              session_id: sessionId,
              role: "assistant",
              content: "",
              attachments: [],
              created_at: Date.now(),
            },
          ],
    );
    return assistantMessageId;
  }

  async function persistTerminalRun(
    sessionId: string,
    run: HermesActiveRun,
    currentMessages: ChannelMessage[],
  ) {
    if (run.output_message_id) {
      const outputMessage = currentMessages.find((message) => message.id === run.output_message_id);
      if (outputMessage) {
        updateMessagesForSession(sessionId, (current) => upsertMessage(current, outputMessage));
        return;
      }
    }

    const existing = currentMessages.find(
      (message) => message.client_message_key === hermesRunMessageKey(run.run_id),
    );
    if (existing) {
      updateMessagesForSession(sessionId, (current) => upsertMessage(current, existing));
      return;
    }

    if (run.status !== "failed" && !run.output) {
      // completed 事件可能先于最终 message_created 到达；没有 output_message_id 时等待
      // 后续消息或重连 snapshot，不能主动制造一个空回答气泡。
      return;
    }

    const content = run.status === "failed"
      ? t("chat.runFailed", { message: run.error || t("chat.requestFailed") })
      : run.output || t("chat.emptyResponse");
    if (currentMessages.some((message) => message.role === "assistant" && message.content === content)) {
      return;
    }

    const executionMessage = await persistExecutionHistoryMessage(sessionId, run.run_id);
    const assistantMessage = await apiClient.appendSessionMessagePublic(sessionId, {
      role: "assistant",
      content,
      attachments: [],
    });
    updateMessagesForSession(sessionId, (current) =>
      completePendingRunMessages(
        current,
        pendingAssistantMessageIdRef.current ?? "",
        executionMessage,
        assistantMessage,
      ),
    );
  }

  async function sendPrompt(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await submitPrompt();
  }

  async function submitPrompt() {
    if (!selectedSession && (!prompt.trim() && attachments.length === 0)) {
      return;
    }

    setBusy(true);
    setError(null);

    const text = prompt.trim();
    const nextAttachments = attachments;
    setPrompt("");
    setAttachments([]);
    resetVerboseEvents();
    stickToBottomRef.current = true;
    // 发送后立即把焦点还给输入框，Hermes 回复时用户也可以继续写下一条草稿。
    focusComposerInputSoon();
    let sessionForRequest: SessionSummary | null = null;
    let assistantMessageId: string | null = null;
    const userMessageKey = createClientMessageId();

    try {
      const session = selectedSession ?? (await createSession());
      if (!session) {
        throw new Error(t("chat.sessionCreateFailed"));
      }
      sessionForRequest = session;
      clearExecutionHistory(session.id);

      const nextAssistantMessageId = createClientMessageId();
      assistantMessageId = nextAssistantMessageId;
      // 真实 Hermes 响应到达前先落一个临时气泡，用它展示输入/回复状态。
      markPendingAssistantMessage(session.id, nextAssistantMessageId);
      updateMessagesForSession(session.id, (current) => [
        ...current,
        {
          id: nextAssistantMessageId,
          session_id: session.id,
          role: "assistant",
          content: "",
          attachments: [],
          created_at: Date.now(),
        },
      ]);
      const userMessage = await apiClient.appendSessionMessagePublic(session.id, {
        role: "user",
        content: text,
        attachments: nextAttachments,
        clientMessageKey: userMessageKey,
      });
      updateMessagesForSession(session.id, (current) =>
        mergeMessagesById(current, [userMessage]),
      );

    } catch (cause) {
      const message = hermesRunErrorMessage(cause, t("chat.requestFailed"), t);
      if (sessionForRequest && assistantMessageId) {
        await appendHermesErrorMessage(sessionForRequest.id, assistantMessageId, message);
      } else {
        if (!sessionForRequest || selectedSessionIdRef.current === sessionForRequest.id) {
          setError(message);
          clearPendingAssistantMessage();
        }
      }
    } finally {
      setBusy(false);
    }
  }

  async function appendHermesErrorMessage(
    sessionId: string,
    assistantMessageId: string,
    message: string,
  ) {
    const content = t("chat.runFailed", { message });
    const executionMessage = await persistExecutionHistoryMessage(
      sessionId,
      activeRunRef.current?.run_id,
    );
    const assistantMessage = await apiClient.appendSessionMessagePublic(sessionId, {
      role: "assistant",
      content,
      attachments: [],
    });
    updateMessagesForSession(sessionId, (current) =>
      completePendingRunMessages(current, assistantMessageId, executionMessage, assistantMessage),
    );
    if (selectedSessionIdRef.current === sessionId) {
      clearPendingAssistantMessage();
      setActiveRun(null);
      clearExecutionHistory(sessionId);
    }
    clearExecutionHistory(sessionId);
  }

  function updateMessagesForSession(
    sessionId: string,
    updater: (current: ChannelMessage[]) => ChannelMessage[],
  ) {
    if (selectedSessionIdRef.current === sessionId) {
      setMessages(updater);
    }
  }

  async function appendVerboseEvent(
    sessionId: string,
    message: HermesVerboseEvent | string,
  ) {
    const event = normalizeExecutionEntry(message);
    if (!event) {
      return;
    }

    const current = currentExecutionEventsForSession(sessionId);
    if (sameExecutionEntry(current.at(-1), event)) {
      return;
    }

    const next = mergeExecutionEvents(current, [event]);
    verboseEventsRef.current = {
      ...verboseEventsRef.current,
      [sessionId]: next,
    };
    if (selectedSessionIdRef.current === sessionId) {
      setVerboseEvents(next);
    }
    await persistExecutionHistoryMessage(sessionId, activeRunRef.current?.run_id);
  }

  function resetVerboseEvents() {
    setVerboseEvents([]);
  }

  function currentExecutionEventsForSession(sessionId: string) {
    return verboseEventsRef.current[sessionId] ?? [];
  }

  function hydrateExecutionHistory(
    sessionId: string,
    sessionMessages: ChannelMessage[],
    run: HermesActiveRun | null,
  ) {
    const executionMessage = activeExecutionMessageForRun(sessionMessages, run);
    const currentEvents = verboseEventsRef.current[sessionId] ?? [];
    if (currentEvents.length > 0) {
      verboseEventsRef.current[sessionId] = currentEvents;
    }
    if (!executionMessage) {
      // 切换会话时如果当前 run 还有未完成的执行过程，先保留内存中的事件，
      // 等待异步落库完成后再由当前 run 的持久化内容接管，避免还没写入时被误删。
      if (currentEvents.length > 0) {
        if (selectedSessionIdRef.current === sessionId) {
          setVerboseEvents(currentEvents);
        }
        return;
      }
      delete verboseEventsRef.current[sessionId];
      return;
    }

    const persistedEvents = executionHistoryEvents(executionMessage.content) ?? [];
    // 当前 run 的执行步骤可能同时来自已落库消息和当前 SSE 流；按顺序去重合并，避免切会话或刷新时丢第一条。
    const mergedEvents = mergeExecutionEvents(persistedEvents, currentEvents);
    verboseEventsRef.current[sessionId] = mergedEvents;
    if (selectedSessionIdRef.current === sessionId) {
      setVerboseEvents(mergedEvents);
    }
  }

  function clearExecutionHistory(sessionId: string) {
    delete verboseEventsRef.current[sessionId];
    delete executionPersistQueueRef.current[sessionId];
    if (selectedSessionIdRef.current === sessionId) {
      setVerboseEvents([]);
    }
  }

  async function persistExecutionHistoryMessage(sessionId: string, runId?: string | null) {
    const events = currentExecutionEventsForSession(sessionId);
    const previous = executionPersistQueueRef.current[sessionId] ?? Promise.resolve(null);
    const next = previous
      .catch(() => null)
      .then(() => persistExecutionHistoryMessageNow(sessionId, events, runId));
    executionPersistQueueRef.current[sessionId] = next;
    return next;
  }

  async function persistExecutionHistoryMessageNow(
    sessionId: string,
    events: ExecutionHistoryEntry[],
    runId?: string | null,
  ) {
    if (events.length === 0) {
      return null;
    }

    const content = executionHistoryContent(events);
    const clientMessageKey = runId && isHubRunId(runId) ? hermesExecutionMessageKey(runId) : undefined;
    const existingMessage = clientMessageKey
      ? messagesRef.current.find(
          (message) =>
            message.session_id === sessionId && message.client_message_key === clientMessageKey,
        )
      : null;
    const message = existingMessage
      ? await apiClient.updateSessionMessagePublic(sessionId, existingMessage.id, {
          content,
          attachments: [],
        })
      : await apiClient.appendSessionMessagePublic(sessionId, {
          role: "assistant",
          content,
          attachments: [],
          clientMessageKey,
        });

    updateMessagesForSession(sessionId, (current) => upsertExecutionMessage(current, message));
    return message;
  }

  async function stopCurrentRun() {
    if (!selectedSession || !activeRun) {
      return;
    }

    setError(null);
    try {
      const sessionId = selectedSession.id;
      await apiClient.stopSessionRunPublic(sessionId);
      const pendingId = pendingAssistantMessageId;
      const pendingMessage = pendingId
        ? messages.find((message) => message.id === pendingId)
        : undefined;
      if (pendingId && pendingMessage?.content.trim()) {
        const executionMessage = await persistExecutionHistoryMessage(sessionId, activeRun.run_id);
        const assistantMessage = await apiClient.appendSessionMessagePublic(sessionId, {
          role: "assistant",
          content: pendingMessage.content,
          attachments: [],
        });
        updateMessagesForSession(sessionId, (current) =>
          completePendingRunMessages(current, pendingId, executionMessage, assistantMessage),
        );
      } else if (pendingId) {
        const executionMessage = await persistExecutionHistoryMessage(sessionId, activeRun.run_id);
        updateMessagesForSession(sessionId, (current) => {
          const withoutPending = current.filter((message) => message.id !== pendingId);
          return executionMessage ? upsertMessage(withoutPending, executionMessage) : withoutPending;
        });
      }
      if (selectedSessionIdRef.current === sessionId) {
        clearPendingAssistantMessage();
        setActiveRun(null);
        clearExecutionHistory(sessionId);
      }
      delete pendingAssistantIdsByRunRef.current[activeRun.run_id];
      clearExecutionHistory(sessionId);
      setBusy(false);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.stopFailed"));
    }
  }

  async function deleteSidebarSession(session: SessionSummary) {
    setError(null);
    try {
      await apiClient.deleteSessionPublic(session.id);
      const nextSessions = sessions.filter((item) => item.id !== session.id);
      setSessions(nextSessions);
      setSeenSessionUpdates((current) => {
        const next = { ...current };
        delete next[session.id];
        return next;
      });
      setUnreadSessionIds((current) => withoutSession(current, session.id));
      if (selectedSession?.id === session.id) {
        const nextSelected = nextSessions[0] ?? null;
        selectedSessionIdRef.current = nextSelected?.id ?? null;
        setSelectedSession(nextSelected);
        setMessages([]);
        clearPendingAssistantMessage();
        setActiveRun(null);
        resetVerboseEvents();
      }
      clearExecutionHistory(session.id);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : t("chat.deleteFailed"));
    }
  }

  async function pickFiles(files: FileList | null) {
    if (!files?.length) {
      return;
    }
    setError(null);
    try {
      const session = selectedSession ?? (await createSession());
      if (!session) {
        throw new Error(t("chat.sessionCreateFailed"));
      }
      const selected = await apiClient.uploadSessionAttachmentsPublic(session.id, Array.from(files));
      setAttachments((current) => [...current, ...selected]);
      if (fileInputRef.current) {
        fileInputRef.current.value = "";
      }
    } catch (cause) {
      setError(userFacingErrorMessage(cause, t("chat.attachmentUploadFailed"), t));
    }
  }

  if (!active) {
    return null;
  }

  // 会话切换和 SSE snapshot 都是异步状态更新；渲染层再按当前会话兜底过滤，
  // 避免旧会话消息在新会话页面短暂或异常残留。
  const selectedSessionId = selectedSession?.id ?? null;
  const renderedMessages = sortMessagesForDisplay(
    messages.filter((message) => !selectedSessionId || message.session_id === selectedSessionId),
  );
  const runInProgress = Boolean(activeRun && !isTerminalHermesRun(activeRun));
  const liveExecutionVisible = Boolean(
    pendingAssistantSessionId === selectedSession?.id && verboseEvents.length > 0,
  );
  const activeExecutionMessage = activeExecutionMessageForRun(renderedMessages, activeRun);

  return (
    <section className="chat-workspace">
      <main className="chat-panel" aria-labelledby="chat-title">
        <header className="chat-header">
          <div className="chat-title-row">
            <h2 id="chat-title">{selectedSession?.title ?? t("chat.newConversation")}</h2>
            {runInProgress ? (
              <span className="header-typing" aria-live="polite">
                <span className="typing-dots" aria-hidden="true">
                  <i />
                  <i />
                  <i />
                </span>
                {t("chat.typing")}
              </span>
            ) : null}
          </div>
          <button type="button" className="secondary" onClick={() => void refreshSessions()}>
            <RefreshCw aria-hidden="true" size={16} />
            {t("chat.refresh")}
          </button>
        </header>

        <div
          className={renderedMessages.length === 0 ? "message-list empty" : "message-list"}
          ref={messageListRef}
        >
          {renderedMessages.length === 0 ? (
            <div className="empty-chat">
              <Bot aria-hidden="true" size={30} />
              <strong>{t("chat.empty")}</strong>
            </div>
          ) : (
            renderedMessages.map((message) => {
              const isPendingMessage = message.id === pendingAssistantMessageId;
              if (
                liveExecutionVisible &&
                activeExecutionMessage?.id === message.id &&
                !isPendingMessage
              ) {
                return null;
              }
              if (!shouldRenderMessageBubble(message, isPendingMessage)) {
                return null;
              }

              return (
                <MessageBubble
                  key={message.id}
                  message={message}
                  pending={isPendingMessage}
                  executionEvents={
                    isPendingMessage && verboseEvents.length > 0 ? verboseEvents : undefined
                  }
                  onPreviewImage={setPreviewAttachment}
                  t={t}
                />
              );
            })
          )}
        </div>

        <form className="composer" onSubmit={sendPrompt}>
          {error ? <p className="error">{error}</p> : null}
          {attachments.length > 0 ? (
            <div className="attachment-row">
              {attachments.map((attachment) => (
                <span key={`${attachment.id ?? attachment.name}-${attachment.size ?? 0}`}>
                  {attachmentIcon(attachment, 15)}
                  {attachment.name}
                </span>
              ))}
            </div>
          ) : null}
          <textarea
            ref={composerInputRef}
            aria-label={t("chat.messageLabel")}
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            placeholder={t("chat.messagePlaceholder")}
            onKeyDown={(event) => {
              // 中文/日文等输入法候选确认时也会触发 Enter，必须让组合输入先完成。
              if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) {
                return;
              }
              if (!busy) {
                event.preventDefault();
                void submitPrompt();
              }
            }}
          />
          <div className="composer-actions">
            <input
              ref={fileInputRef}
              type="file"
              multiple
              hidden
              onChange={(event) => void pickFiles(event.target.files)}
            />
            <button
              type="button"
              className="secondary icon-text attach-button"
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip aria-hidden="true" size={17} />
              {t("chat.attach")}
            </button>
            <button
              type="button"
              className="secondary icon-text"
              disabled={!runInProgress}
              onClick={() => void stopCurrentRun()}
            >
              <CircleStop aria-hidden="true" size={17} />
              {t("chat.stop")}
            </button>
            <button
              type="submit"
              disabled={busy || (!prompt.trim() && attachments.length === 0)}
            >
              <Send aria-hidden="true" size={17} />
              {t("chat.send")}
            </button>
          </div>
        </form>
      </main>
      {previewAttachment ? (
        <ImagePreviewDialog
          attachment={previewAttachment}
          onClose={() => setPreviewAttachment(null)}
          t={t}
        />
      ) : null}
    </section>
  );
}

const EXECUTION_HISTORY_MARKER = "<!-- hermes-hub:execution:v1 -->";
const LEGACY_HERMES_EXECUTION_LINE = /^\S+\s+([A-Za-z0-9_.-]+)\((.*)\)$/u;

function normalizeExecutionEntry(message: HermesVerboseEvent | string): ExecutionHistoryEntry | null {
  if (typeof message === "string") {
    const detail = normalizeExecutionText(message);
    return detail ? { kind: "text", detail } : null;
  }

  const detail = normalizeExecutionText(message.detail);
  const tool = normalizeExecutionText(message.tool);
  const choice = normalizeExecutionText(message.choice);
  return {
    kind: message.kind,
    ...(tool ? { tool } : {}),
    ...(detail ? { detail } : {}),
    ...(choice ? { choice } : {}),
    ...(message.failed ? { failed: true } : {}),
  };
}

function normalizeExecutionText(message: string | undefined) {
  return message?.replace(/\s+/g, " ").trim() || "";
}

function sameExecutionEntry(
  left: ExecutionHistoryEntry | undefined,
  right: ExecutionHistoryEntry,
) {
  return Boolean(left) && JSON.stringify(left) === JSON.stringify(right);
}

function mergeExecutionEvents(
  ...eventGroups: Array<ExecutionHistoryEntry[] | undefined>
): ExecutionHistoryEntry[] {
  let merged: ExecutionHistoryEntry[] = [];

  for (const group of eventGroups) {
    const events = group ?? [];
    if (events.length === 0) {
      continue;
    }

    // 同一组里的重复步骤可能是真实连续执行；只去掉不同来源之间的边界重叠，
    // 例如 SSE 实时消息和重连 snapshot 同时包含同一段执行历史。
    const overlap = overlappingExecutionPrefixLength(merged, events);
    merged = [...merged, ...events.slice(overlap)];
  }

  return merged;
}

function overlappingExecutionPrefixLength(
  previous: ExecutionHistoryEntry[],
  next: ExecutionHistoryEntry[],
) {
  let overlap = Math.min(previous.length, next.length);
  while (overlap > 0) {
    const previousTail = previous.slice(previous.length - overlap);
    const nextHead = next.slice(0, overlap);
    if (sameExecutionSequence(previousTail, nextHead)) {
      return overlap;
    }
    overlap -= 1;
  }
  return 0;
}

function sameExecutionSequence(left: ExecutionHistoryEntry[], right: ExecutionHistoryEntry[]) {
  return (
    left.length === right.length &&
    left.every((entry, index) =>
      right[index] ? sameExecutionEntry(entry, right[index]) : false,
    )
  );
}

function compactExecutionDisplayLine(message: string) {
  const normalized = message.replace(/\s+/g, " ").trim();
  if (!normalized) {
    return "";
  }

  const chars = Array.from(normalized);
  return chars.length > 50 ? `${chars.slice(0, 49).join("")}…` : normalized;
}

function compactToolResultDetail(message: string) {
  const normalized = message.replace(/\s+/g, " ").trim();
  if (!normalized) {
    return "";
  }

  const chars = Array.from(normalized);
  return chars.length > 50 ? `${chars.slice(0, 50).join("")}…` : normalized;
}

function compactToolParameterDetail(message: string) {
  const normalized = message.replace(/\s+/g, " ").trim();
  if (!normalized) {
    return "";
  }

  const chars = Array.from(normalized);
  return chars.length > 50 ? `${chars.slice(0, 50).join("")}…` : normalized;
}

function executionHistoryContent(events: ExecutionHistoryEntry[]) {
  return `${EXECUTION_HISTORY_MARKER}\n${JSON.stringify(events)}`;
}

function completePendingRunMessages(
  messages: ChannelMessage[],
  pendingMessageId: string,
  executionMessage: ChannelMessage | null,
  finalMessage: ChannelMessage,
) {
  const next = messages.filter((message) => message.id !== pendingMessageId);
  return upsertMessage(executionMessage ? upsertMessage(next, executionMessage) : next, finalMessage);
}

function upsertMessage(messages: ChannelMessage[], nextMessage: ChannelMessage) {
  if (messages.some((message) => message.id === nextMessage.id)) {
    return messages.map((message) => (message.id === nextMessage.id ? nextMessage : message));
  }
  return [...messages, nextMessage];
}

function mergeMessagesById(messages: ChannelMessage[], nextMessages: ChannelMessage[]) {
  return sortMessagesForDisplay(
    nextMessages.reduce((current, message) => upsertMessage(current, message), messages),
  );
}

function upsertExecutionMessage(messages: ChannelMessage[], nextMessage: ChannelMessage) {
  const withoutExisting = messages.filter((message) => message.id !== nextMessage.id);
  const pendingIndex = withoutExisting.findIndex((message) => message.id.startsWith("pending-"));
  if (pendingIndex === -1) {
    return [...withoutExisting, nextMessage];
  }
  return [
    ...withoutExisting.slice(0, pendingIndex),
    nextMessage,
    ...withoutExisting.slice(pendingIndex),
  ];
}

function isMessageListNearBottom(node: HTMLElement) {
  return node.scrollHeight - node.scrollTop - node.clientHeight <= 72;
}

function scrollMessageListToBottom(node: HTMLElement | null) {
  if (!node) {
    return;
  }

  const schedule = globalThis.requestAnimationFrame ?? ((callback: FrameRequestCallback) => {
    globalThis.setTimeout(callback, 0);
    return 0;
  });
  schedule(() => {
    node.scrollTop = node.scrollHeight;
  });
}

export function createClientMessageId(source: BrowserCrypto | undefined = globalThis.crypto) {
  if (typeof source?.randomUUID === "function") {
    return source.randomUUID();
  }

  if (typeof source?.getRandomValues === "function") {
    const bytes = source.getRandomValues(new Uint8Array(16));
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  return `msg-${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
}

function ChatSidebar({
  sessions,
  selectedSession,
  collapsed,
  unreadSessionIds,
  onCreate,
  onSelect,
  onDelete,
}: {
  sessions: SessionSummary[];
  selectedSession: SessionSummary | null;
  collapsed: boolean;
  unreadSessionIds: Set<string>;
  onCreate: () => void;
  onSelect: (session: SessionSummary) => void;
  onDelete: (session: SessionSummary) => void;
}) {
  const { t } = useI18n();
  const listRef = useRef<HTMLUListElement | null>(null);
  const [scrollbar, setScrollbar] = useState({
    visible: false,
    thumbHeight: 0,
    thumbTop: 0,
  });

  useEffect(() => {
    const list = listRef.current;
    if (!list) {
      return;
    }

    function updateScrollbar() {
      const list = listRef.current;
      if (!list || list.scrollHeight <= list.clientHeight + 1) {
        setScrollbar((current) =>
          current.visible ? { visible: false, thumbHeight: 0, thumbTop: 0 } : current,
        );
        return;
      }

      const trackHeight = list.clientHeight;
      const thumbHeight = Math.max(28, (list.clientHeight / list.scrollHeight) * trackHeight);
      const maxScrollTop = list.scrollHeight - list.clientHeight;
      const maxThumbTop = Math.max(0, trackHeight - thumbHeight);
      const thumbTop = maxScrollTop > 0 ? (list.scrollTop / maxScrollTop) * maxThumbTop : 0;
      setScrollbar({ visible: true, thumbHeight, thumbTop });
    }

    updateScrollbar();
    list.addEventListener("scroll", updateScrollbar, { passive: true });
    window.addEventListener("resize", updateScrollbar);

    // 会话列表高度会被管理菜单和账号区挤压，观察尺寸变化才能让悬浮滚动条保持准确。
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(updateScrollbar);
    observer?.observe(list);

    return () => {
      list.removeEventListener("scroll", updateScrollbar);
      window.removeEventListener("resize", updateScrollbar);
      observer?.disconnect();
    };
  }, [sessions.length]);

  return (
    <div className="chat-sidebar-menu">
      {/* 频道是固定系统通道，侧栏只保留用户真正操作的会话入口。 */}
      <button type="button" onClick={onCreate} title={t("chat.newChat")}>
        <Plus aria-hidden="true" size={17} />
        <span>{t("chat.newChat")}</span>
      </button>
      <div className="session-list-wrap">
        <ul className="session-list" ref={listRef}>
          {sessions.map((session) => (
            <li key={session.id}>
              {/*
                非当前会话有新消息时只做视觉提醒，不改变 selectedSession，
                避免用户正在看的会话被列表刷新抢走。
              */}
              <div
                className={[
                  "session-row",
                  selectedSession?.id === session.id ? "active" : "",
                  unreadSessionIds.has(session.id) ? "unread" : "",
                ]
                  .filter(Boolean)
                  .join(" ")}
              >
                <button
                  type="button"
                  className={[
                    "session-select",
                    selectedSession?.id === session.id ? "active" : "",
                    unreadSessionIds.has(session.id) ? "unread" : "",
                  ]
                    .filter(Boolean)
                    .join(" ")}
                  onClick={() => onSelect(session)}
                  title={session.title ?? t("chat.newConversation")}
                >
                  <MessageSquare aria-hidden="true" size={15} />
                  <span className="session-title">{session.title ?? t("chat.newConversation")}</span>
                </button>
                {!collapsed ? (
                  <button
                    type="button"
                    className="session-delete"
                    aria-label={t("chat.deleteSession")}
                    onClick={() => onDelete(session)}
                  >
                    <Trash2 aria-hidden="true" size={15} />
                  </button>
                ) : null}
              </div>
            </li>
          ))}
        </ul>
        {scrollbar.visible ? (
          <div className="session-scrollbar" aria-hidden="true">
            <span
              className="session-scrollbar-thumb"
              style={{
                height: `${scrollbar.thumbHeight}px`,
                transform: `translateY(${scrollbar.thumbTop}px)`,
              }}
            />
          </div>
        ) : null}
      </div>
    </div>
  );
}

function sessionUpdatedAt(session: SessionSummary) {
  return session.updated_at ?? session.created_at ?? 0;
}

function activeExecutionMessageForRun(
  messages: ChannelMessage[],
  run: HermesActiveRun | null,
) {
  if (!run || isTerminalHermesRun(run)) {
    return null;
  }

  const executionKey = hermesExecutionMessageKey(run.run_id);
  const keyedMessage = messages.find(
    (message) =>
      message.client_message_key === executionKey && isExecutionHistoryContent(message.content),
  );
  if (keyedMessage) {
    return keyedMessage;
  }

  const runCreatedAt = run.created_at ?? 0;
  return (
    [...messages]
      .reverse()
      .find(
        (message) =>
          isExecutionHistoryContent(message.content) &&
          (message.created_at ?? 0) >= runCreatedAt,
      ) ?? null
  );
}

function isTerminalHermesRun(run: HermesActiveRun) {
  return ["completed", "failed", "cancelled", "canceled", "stopped"].includes(run.status);
}

function isHubRunId(runId: string) {
  return runId.startsWith("hub-run-");
}

function activeRunFromChannelRun(run: ChannelRun): HermesActiveRun {
  return {
    run_id: run.run_id,
    status: run.status,
    error: run.error,
    output_message_id: run.output_message_id,
    created_at: run.created_at,
    updated_at: run.updated_at,
  };
}

function hasRenderableMessageBody(message: ChannelMessage) {
  return Boolean(
    message.content.trim() ||
      (message.attachments ?? []).length > 0 ||
      isExecutionHistoryContent(message.content),
  );
}

function shouldRenderMessageBubble(message: ChannelMessage, pending: boolean) {
  if (message.role !== "assistant") {
    return true;
  }
  return pending || hasRenderableMessageBody(message);
}

function removePendingAssistantForMessage(
  messages: ChannelMessage[],
  message: ChannelMessage,
  pendingMessageId?: string,
) {
  if (message.role !== "assistant") {
    return messages;
  }
  return messages.filter(
    (current) =>
      !(
        current.role === "assistant" &&
        current.session_id === message.session_id &&
        !current.content.trim() &&
        (current.attachments ?? []).length === 0 &&
        (current.id.startsWith("pending-") ||
          current.id === pendingMessageId ||
          !current.client_message_key)
      ),
  );
}

function hermesRunErrorMessage(
  cause: unknown,
  fallback = "Hermes request failed",
  t?: Translate,
) {
  if (t) {
    return userFacingErrorMessage(cause, fallback, t);
  }

  if (cause instanceof Error && cause.message.trim()) {
    return cause.message;
  }

  return fallback;
}

function userFacingErrorMessage(cause: unknown, fallback: string, t: Translate) {
  if (cause instanceof ApiRequestError && cause.code === "session_limit_exceeded") {
    return t("chat.sessionLimitExceeded", { count: cause.maxSessionsPerUser ?? 20 });
  }

  if (cause instanceof Error && cause.message.trim()) {
    return cause.message;
  }

  return fallback;
}

function hermesRunMessageKey(runId: string) {
  return `hermes-run:${runId}`;
}

function hermesExecutionMessageKey(runId: string) {
  return `hermes-execution:${runId}`;
}

function runIdFromHermesMessageKey(value: string | null | undefined) {
  const raw = value?.trim();
  if (!raw?.startsWith("hermes-run:")) {
    return null;
  }
  const runId = raw.slice("hermes-run:".length).split(":")[0];
  return runId.startsWith("hub-run-") ? runId : null;
}

function withoutSession(source: Set<string>, sessionId: string) {
  if (!source.has(sessionId)) {
    return source;
  }
  const next = new Set(source);
  next.delete(sessionId);
  return next;
}

function MessageBubble({
  message,
  pending = false,
  executionEvents: executionEventsOverride,
  onPreviewImage,
  t,
}: {
  message: ChannelMessage;
  pending?: boolean;
  executionEvents?: ExecutionHistoryEntry[];
  onPreviewImage: (attachment: HermesAttachment) => void;
  t: Translate;
}) {
  const executionEvents = executionEventsOverride ?? executionHistoryEvents(message.content);
  const hasExecutionEvents = Array.isArray(executionEvents) && executionEvents.length > 0;
  const hasVisibleBody = Boolean(
    message.content || message.attachments?.length || hasExecutionEvents || pending,
  );
  const attachments = message.attachments ?? [];

  return (
    <article
      className={[
        "message-bubble",
        message.role,
        pending ? "pending" : "",
        hasVisibleBody ? "has-content" : "empty-body",
      ]
        .filter(Boolean)
        .join(" ")}
    >
      {hasExecutionEvents ? (
        <div className="execution-history">
          <strong>{t("chat.executionSteps")}</strong>
          <ul className="verbose-log" aria-label={t("chat.hermesLog")}>
            {executionEvents.map((event, index) => (
              <li key={`execution-${index}`}>{formatExecutionEntry(event, t)}</li>
            ))}
          </ul>
        </div>
      ) : message.content ? (
        <MarkdownContent
          content={message.content}
          attachments={attachments}
          onPreviewImage={onPreviewImage}
          t={t}
        />
      ) : attachments.length ? (
        <InlineAttachments
          attachments={attachments}
          referencedAttachmentIds={new Set()}
          onPreviewImage={onPreviewImage}
          t={t}
        />
      ) : null}
      {pending ? (
        <div
          className="typing-indicator"
          aria-label={message.content ? t("chat.replying") : t("chat.typing")}
          aria-live="polite"
        >
          <span className="typing-dots" aria-hidden="true">
            <i />
            <i />
            <i />
          </span>
        </div>
      ) : null}
    </article>
  );
}

function sortMessagesForDisplay(messages: ChannelMessage[]) {
  const sorted = mergeExecutionHistoryMessages(messages)
    .map((message, index) => ({ message, index }))
    .sort((left, right) => {
      const leftCreatedAt = left.message.created_at ?? 0;
      const rightCreatedAt = right.message.created_at ?? 0;
      if (leftCreatedAt !== rightCreatedAt) {
        return leftCreatedAt - rightCreatedAt;
      }

      const leftExecution = isExecutionHistoryContent(left.message.content) ? 0 : 1;
      const rightExecution = isExecutionHistoryContent(right.message.content) ? 0 : 1;
      if (leftExecution !== rightExecution) {
        return leftExecution - rightExecution;
      }

      return left.index - right.index;
    })
    .map(({ message }) => message);
  return dedupeRepeatedAssistantMessages(sorted);
}

function mergeExecutionHistoryMessages(messages: ChannelMessage[]) {
  // 执行块按消息逐条保留，不再把不同轮次的执行过程折叠成一条。
  return messages;
}

function isExecutionHistoryContent(content: string) {
  return (
    content.startsWith(`${EXECUTION_HISTORY_MARKER}\n`) ||
    content.startsWith("执行步骤\n") ||
    Boolean(parseLegacyHermesExecutionEvents(content))
  );
}

function dedupeRepeatedAssistantMessages(messages: ChannelMessage[]) {
  const deduped: ChannelMessage[] = [];

  for (const message of messages) {
    const previous = deduped.at(-1);
    if (previous && isRepeatedAssistantMessage(previous, message)) {
      continue;
    }
    deduped.push(message);
  }

  return deduped;
}

function isRepeatedAssistantMessage(left: ChannelMessage, right: ChannelMessage) {
  return (
    left.role === "assistant" &&
    right.role === "assistant" &&
    !isExecutionHistoryContent(left.content) &&
    !isExecutionHistoryContent(right.content) &&
    left.content.trim() !== "" &&
    left.content.trim() === right.content.trim()
  );
}

function formatExecutionEntry(event: ExecutionHistoryEntry, t: Translate) {
  const tool = localizeToolName(event.tool, t);
  const detail = localizeKnownToolNames(event.detail ?? "", t);

  if (event.kind === "text") {
    return compactExecutionDisplayLine(detail);
  }
  if (event.kind === "approval.request") {
    return compactExecutionDisplayLine(joinExecutionParts(t("execution.waiting"), detail));
  }
  if (event.kind === "approval.responded") {
    return compactExecutionDisplayLine(
      joinExecutionParts(t("execution.approved"), event.choice ?? "session"),
    );
  }
  if (event.kind === "tool.call") {
    return joinExecutionParts(`${t("execution.call")} ${tool}`, compactToolParameterDetail(detail));
  }
  if (event.kind === "tool.completed") {
    const action = event.failed ? t("execution.failed") : t("execution.completed");
    return joinExecutionParts(`${action} ${tool}`, compactToolResultDetail(detail));
  }
  if (event.kind === "tool.progress") {
    return compactExecutionDisplayLine(joinExecutionParts(`${t("execution.progress")} ${tool}`, detail));
  }
  return compactExecutionDisplayLine(joinExecutionParts(`${t("execution.started")} ${tool}`, detail));
}

function joinExecutionParts(prefix: string, detail: string) {
  return detail ? `${prefix}：${detail}` : prefix;
}

function localizeToolName(tool: string | undefined, t: Translate) {
  const raw = normalizeExecutionText(tool);
  if (!raw) {
    return t("tool.unknown");
  }
  const normalized = raw.toLowerCase().replace(/[\s-]+/g, "_");
  switch (normalized) {
    case "image_generate":
    case "image_generation":
    case "generate_image":
    case "image":
    case "图片生成":
      return t("tool.imageGenerate");
    case "skill_view":
    case "skill":
    case "技能查看":
    case "技能视图":
      return t("tool.skillView");
    case "terminal":
    case "term":
    case "终端":
      return t("tool.terminal");
    case "shell":
    case "bash":
      return t("tool.shell");
    case "command":
    case "cmd":
    case "命令":
      return t("tool.command");
    case "browser":
    case "浏览器":
      return t("tool.browser");
    case "file":
    case "文件":
      return t("tool.file");
    case "tool":
    case "工具":
      return t("tool.unknown");
    default:
      return raw;
  }
}

function localizeKnownToolNames(text: string, t: Translate) {
  if (!text) {
    return "";
  }
  return text
    .replace(/\bimage_generate\b/gi, t("tool.imageGenerate"))
    .replace(/\bskill_view\b/gi, t("tool.skillView"))
    .replace(/\bterminal\b/gi, t("tool.terminal"))
    .replace(/图片生成/g, t("tool.imageGenerate"))
    .replace(/技能查看|技能视图/g, t("tool.skillView"))
    .replace(/终端/g, t("tool.terminal"));
}

function MarkdownContent({
  content,
  attachments = [],
  onPreviewImage,
  t,
}: {
  content: string;
  attachments?: HermesAttachment[];
  onPreviewImage: (attachment: HermesAttachment) => void;
  t: Translate;
}) {
  const referencedAttachmentIds = new Set<string>();

  return (
    <div className="markdown-content">
      {parseMarkdownBlocks(content).map((block, index) =>
        renderMarkdownBlock(block, index, attachments, referencedAttachmentIds, onPreviewImage, t),
      )}
      <InlineAttachments
        attachments={attachments}
        referencedAttachmentIds={referencedAttachmentIds}
        onPreviewImage={onPreviewImage}
        t={t}
      />
    </div>
  );
}

type MarkdownBlock =
  | { type: "code"; text: string; language?: string }
  | { type: "heading"; level: number; text: string }
  | { type: "paragraph"; text: string }
  | { type: "ul"; items: string[] }
  | { type: "ol"; items: string[] }
  | { type: "quote"; text: string };

function parseMarkdownBlocks(content: string): MarkdownBlock[] {
  const blocks: MarkdownBlock[] = [];
  const lines = content.replace(/\r\n/g, "\n").split("\n");

  for (let index = 0; index < lines.length; ) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }

    const fence = line.match(/^```(\S*)\s*$/);
    if (fence) {
      const codeLines: string[] = [];
      index += 1;
      while (index < lines.length && !/^```\s*$/.test(lines[index])) {
        codeLines.push(lines[index]);
        index += 1;
      }
      blocks.push({ type: "code", language: fence[1] || undefined, text: codeLines.join("\n") });
      index += index < lines.length ? 1 : 0;
      continue;
    }

    const heading = line.match(/^(#{1,6})\s+(.+)$/);
    if (heading) {
      blocks.push({ type: "heading", level: heading[1].length, text: heading[2].trim() });
      index += 1;
      continue;
    }

    if (/^\s*[-*]\s+/.test(line)) {
      const items: string[] = [];
      while (index < lines.length && /^\s*[-*]\s+/.test(lines[index])) {
        items.push(lines[index].replace(/^\s*[-*]\s+/, "").trim());
        index += 1;
      }
      blocks.push({ type: "ul", items });
      continue;
    }

    if (/^\s*\d+[.)]\s+/.test(line)) {
      const items: string[] = [];
      while (index < lines.length && /^\s*\d+[.)]\s+/.test(lines[index])) {
        items.push(lines[index].replace(/^\s*\d+[.)]\s+/, "").trim());
        index += 1;
      }
      blocks.push({ type: "ol", items });
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const quoteLines: string[] = [];
      while (index < lines.length && /^\s*>\s?/.test(lines[index])) {
        quoteLines.push(lines[index].replace(/^\s*>\s?/, ""));
        index += 1;
      }
      blocks.push({ type: "quote", text: quoteLines.join("\n") });
      continue;
    }

    const paragraphLines = [line];
    index += 1;
    while (
      index < lines.length &&
      lines[index].trim() &&
      !/^```/.test(lines[index]) &&
      !/^(#{1,6})\s+/.test(lines[index]) &&
      !/^\s*[-*]\s+/.test(lines[index]) &&
      !/^\s*\d+[.)]\s+/.test(lines[index]) &&
      !/^\s*>\s?/.test(lines[index])
    ) {
      paragraphLines.push(lines[index]);
      index += 1;
    }
    blocks.push({ type: "paragraph", text: paragraphLines.join("\n") });
  }

  return blocks.length > 0 ? blocks : [{ type: "paragraph", text: content }];
}

function renderMarkdownBlock(
  block: MarkdownBlock,
  index: number,
  attachments: HermesAttachment[],
  referencedAttachmentIds: Set<string>,
  onPreviewImage: (attachment: HermesAttachment) => void,
  t: Translate,
) {
  if (block.type === "code") {
    return (
      <pre className="markdown-code" key={`code-${index}`}>
        <code>{block.text}</code>
      </pre>
    );
  }

  if (block.type === "heading") {
    const children = renderInlineMarkdown(
      block.text,
      `heading-${index}`,
      attachments,
      referencedAttachmentIds,
      onPreviewImage,
      t,
    );
    if (block.level <= 1) {
      return <h3 key={`heading-${index}`}>{children}</h3>;
    }
    if (block.level === 2) {
      return <h4 key={`heading-${index}`}>{children}</h4>;
    }
    if (block.level === 3) {
      return <h5 key={`heading-${index}`}>{children}</h5>;
    }
    return <h6 key={`heading-${index}`}>{children}</h6>;
  }

  if (block.type === "ul" || block.type === "ol") {
    const Tag = block.type;
    return (
      <Tag key={`${block.type}-${index}`}>
        {block.items.map((item, itemIndex) => (
          <li key={`${block.type}-${index}-${itemIndex}`}>
            {renderInlineMarkdown(
              item,
              `${block.type}-${index}-${itemIndex}`,
              attachments,
              referencedAttachmentIds,
              onPreviewImage,
              t,
            )}
          </li>
        ))}
      </Tag>
    );
  }

  if (block.type === "quote") {
    return (
      <blockquote key={`quote-${index}`}>
        {renderInlineMarkdownWithBreaks(
          block.text,
          `quote-${index}`,
          attachments,
          referencedAttachmentIds,
          onPreviewImage,
          t,
        )}
      </blockquote>
    );
  }

  return (
    <p key={`paragraph-${index}`}>
      {renderInlineMarkdownWithBreaks(
        block.text,
        `paragraph-${index}`,
        attachments,
        referencedAttachmentIds,
        onPreviewImage,
        t,
      )}
    </p>
  );
}

function renderInlineMarkdownWithBreaks(
  text: string,
  keyPrefix: string,
  attachments: HermesAttachment[],
  referencedAttachmentIds: Set<string>,
  onPreviewImage: (attachment: HermesAttachment) => void,
  t: Translate,
) {
  return text.split("\n").flatMap((line, index) => {
    const nodes = renderInlineMarkdown(
      line,
      `${keyPrefix}-${index}`,
      attachments,
      referencedAttachmentIds,
      onPreviewImage,
      t,
    );
    return index === 0 ? nodes : [<br key={`${keyPrefix}-br-${index}`} />, ...nodes];
  });
}

function renderInlineMarkdown(
  text: string,
  keyPrefix: string,
  attachments: HermesAttachment[],
  referencedAttachmentIds: Set<string>,
  onPreviewImage: (attachment: HermesAttachment) => void,
  t: Translate,
): ReactNode[] {
  const nodes: ReactNode[] = [];
  const tokenPattern = /(!\[[^\]]*]\([^)]+\)|\[[^\]]+]\([^)]+\)|https?:\/\/\S+|\/api\/attachments\/\S+\/download|`[^`]+`|\*\*[^*]+\*\*)/g;
  let cursor = 0;
  let match: RegExpExecArray | null;

  while ((match = tokenPattern.exec(text))) {
    if (match.index > cursor) {
      nodes.push(text.slice(cursor, match.index));
    }

    const token = match[0];
    const image = token.match(/^!\[([^\]]*)]\(([^)]+)\)$/);
    const link = token.match(/^\[([^\]]+)]\(([^)]+)\)$/);
    if (image) {
      const [, alt, url] = image;
      const attachment = attachmentForUrl(attachments, url);
      if (attachment) {
        referencedAttachmentIds.add(attachment.id ?? attachment.download_url ?? attachment.name);
        nodes.push(
          <InlineAttachment
            key={`${keyPrefix}-attachment-image-${match.index}`}
            attachment={{ ...attachment, name: alt || attachment.name }}
            onPreviewImage={onPreviewImage}
            t={t}
          />,
        );
        cursor = match.index + token.length;
        continue;
      }
      const imageUrl = safeMarkdownUrl(url, true);
      if (imageUrl) {
        nodes.push(
          <button
            key={`${keyPrefix}-image-${match.index}`}
            type="button"
            className="image-preview-trigger markdown-image-trigger"
            aria-label={t("chat.markdownImage", { name: alt || imageUrl })}
            onClick={() =>
              onPreviewImage({
                name: alt || imageUrl.split("/").pop() || "image",
                content_type: markdownImageContentType(imageUrl),
                kind: "image",
                download_url: imageUrl.startsWith("data:") ? undefined : imageUrl,
                data_url: imageUrl.startsWith("data:") ? imageUrl : undefined,
              })
            }
          >
            <img src={imageUrl} alt={alt || imageUrl} />
          </button>,
        );
      } else {
        nodes.push(alt);
      }
    } else if (link) {
      const [, label, url] = link;
      const href = safeMarkdownUrl(url, false);
      const attachment = attachmentForUrl(attachments, url);
      if (attachment) {
        referencedAttachmentIds.add(attachment.id ?? attachment.download_url ?? attachment.name);
        nodes.push(
          <InlineAttachment
            key={`${keyPrefix}-attachment-link-${match.index}`}
            attachment={{ ...attachment, name: label || attachment.name }}
            onPreviewImage={onPreviewImage}
            t={t}
          />,
        );
      } else {
        nodes.push(
          href ? (
            <a key={`${keyPrefix}-link-${match.index}`} href={href} rel="noreferrer" target="_blank">
              {label}
            </a>
          ) : (
            label
          ),
        );
      }
    } else if (isPlainAttachmentUrl(token)) {
      const attachment = attachmentForUrl(attachments, token);
      if (attachment) {
        referencedAttachmentIds.add(attachment.id ?? attachment.download_url ?? attachment.name);
        nodes.push(
          <InlineAttachment
            key={`${keyPrefix}-attachment-url-${match.index}`}
            attachment={attachment}
            onPreviewImage={onPreviewImage}
            t={t}
          />,
        );
      } else {
        nodes.push(token);
      }
    } else if (token.startsWith("`")) {
      nodes.push(<code key={`${keyPrefix}-code-${match.index}`}>{token.slice(1, -1)}</code>);
    } else if (token.startsWith("**")) {
      nodes.push(
        <strong key={`${keyPrefix}-strong-${match.index}`}>{token.slice(2, -2)}</strong>,
      );
    }
    cursor = match.index + token.length;
  }

  if (cursor < text.length) {
    nodes.push(text.slice(cursor));
  }

  return nodes;
}

function InlineAttachments({
  attachments,
  referencedAttachmentIds,
  onPreviewImage,
  t,
}: {
  attachments: HermesAttachment[];
  referencedAttachmentIds: Set<string>;
  onPreviewImage: (attachment: HermesAttachment) => void;
  t: Translate;
}) {
  const remaining = attachments.filter(
    (attachment) =>
      !referencedAttachmentIds.has(attachment.id ?? attachment.download_url ?? attachment.name),
  );

  if (remaining.length === 0) {
    return null;
  }

  return (
    <span className="inline-attachments">
      {remaining.map((attachment) => (
        <InlineAttachment
          key={attachment.id ?? attachment.download_url ?? attachment.name}
          attachment={attachment}
          onPreviewImage={onPreviewImage}
          t={t}
        />
      ))}
    </span>
  );
}

function InlineAttachment({
  attachment,
  onPreviewImage,
  t,
}: {
  attachment: HermesAttachment;
  onPreviewImage: (attachment: HermesAttachment) => void;
  t: Translate;
}) {
  const imageSrc = attachment.data_url ?? attachment.download_url;
  if ((attachment.kind === "image" || attachment.content_type.startsWith("image/")) && imageSrc) {
    return (
      <button
        type="button"
        className="image-preview-trigger markdown-image-trigger"
        aria-label={t("chat.markdownImage", { name: attachment.name })}
        onClick={() => onPreviewImage(attachment)}
      >
        <img src={imageSrc} alt={attachment.name} />
      </button>
    );
  }

  if (!attachment.download_url) {
    return (
      <span className="file-chip">
        {attachmentIcon(attachment, 16)}
        {attachment.name}
      </span>
    );
  }

  return (
    <a
      className="file-chip"
      href={attachment.download_url}
      aria-label={t("file.download", { name: attachment.name })}
    >
      {attachmentIcon(attachment, 16)}
      {attachment.name}
    </a>
  );
}

function attachmentForUrl(attachments: HermesAttachment[], url: string) {
  const normalizedUrl = normalizeAttachmentUrl(url);
  const normalizedId = attachmentIdFromUrl(normalizedUrl);
  return attachments.find((attachment) => {
    const downloadUrl = normalizeAttachmentUrl(attachment.download_url);
    const dataUrl = normalizeAttachmentUrl(attachment.data_url);
    const attachmentId = attachment.id ?? attachmentIdFromUrl(downloadUrl);
    return Boolean(
      normalizedUrl &&
        (normalizedUrl === downloadUrl ||
          normalizedUrl === dataUrl ||
          (normalizedId && attachmentId === normalizedId)),
    );
  });
}

function normalizeAttachmentUrl(url: string | undefined) {
  const trimmed = url?.trim().replace(/[),.;，。；、]+$/u, "") ?? "";
  if (!trimmed) {
    return "";
  }
  try {
    const parsed = new URL(trimmed, window.location.origin);
    if (parsed.origin === window.location.origin) {
      return `${parsed.pathname}${parsed.search}`;
    }
  } catch {
    return trimmed;
  }
  return trimmed;
}

function attachmentIdFromUrl(url: string) {
  return /\/api\/attachments\/([^/]+)\/download/.exec(url)?.[1] ?? null;
}

function isPlainAttachmentUrl(token: string) {
  // 只有真实附件下载地址才进入附件渲染分支；普通文本不能被相对 URL 解析误判。
  return Boolean(attachmentIdFromUrl(normalizeAttachmentUrl(token)));
}

function safeMarkdownUrl(url: string, imageOnly: boolean) {
  const trimmed = url.trim();
  if (
    trimmed.startsWith("/") ||
    trimmed.startsWith("#") ||
    /^https?:\/\//i.test(trimmed) ||
    (imageOnly && /^data:image\//i.test(trimmed))
  ) {
    return trimmed;
  }
  return null;
}

function markdownImageContentType(url: string) {
  if (url.startsWith("data:")) {
    return url.slice("data:".length).split(";")[0] || "image/*";
  }
  if (/\.jpe?g($|\?)/i.test(url)) {
    return "image/jpeg";
  }
  if (/\.webp($|\?)/i.test(url)) {
    return "image/webp";
  }
  if (/\.gif($|\?)/i.test(url)) {
    return "image/gif";
  }
  if (/\.svg($|\?)/i.test(url)) {
    return "image/svg+xml";
  }
  return "image/png";
}

function attachmentIcon(attachment: HermesAttachment, size = 16) {
  if (attachment.kind === "image" || attachment.content_type.startsWith("image/")) {
    return <FileImage aria-hidden="true" size={size} />;
  }

  const name = attachment.name.toLowerCase();
  const type = attachment.content_type;
  if (/\.(ppt|pptx|key)$/i.test(name)) {
    return <Presentation aria-hidden="true" size={size} />;
  }
  if (/\.(xls|xlsx|csv)$/i.test(name)) {
    return <FileSpreadsheet aria-hidden="true" size={size} />;
  }
  if (/\.(zip|rar|7z|tar|gz)$/i.test(name)) {
    return <FileArchive aria-hidden="true" size={size} />;
  }
  if (/\.(ts|tsx|js|jsx|rs|py|json|html|css|md)$/i.test(name)) {
    return <FileCode2 aria-hidden="true" size={size} />;
  }
  if (type.startsWith("audio/")) {
    return <FileAudio aria-hidden="true" size={size} />;
  }
  if (type.startsWith("video/")) {
    return <FileVideo aria-hidden="true" size={size} />;
  }
  if (type === "application/pdf" || /\.(pdf)$/i.test(name)) {
    return <FileType aria-hidden="true" size={size} />;
  }
  if (type.startsWith("text/") || /\.(txt|doc|docx)$/i.test(name)) {
    return <FileText aria-hidden="true" size={size} />;
  }
  return <File aria-hidden="true" size={size} />;
}

function executionHistoryEvents(content: string): ExecutionHistoryEntry[] | null {
  if (content.startsWith(`${EXECUTION_HISTORY_MARKER}\n`)) {
    try {
      const parsed = JSON.parse(content.slice(EXECUTION_HISTORY_MARKER.length).trim());
      if (!Array.isArray(parsed)) {
        return null;
      }
      const events = parsed
        .map((event) => normalizeStoredExecutionEntry(event))
        .filter((event): event is ExecutionHistoryEntry => Boolean(event));
      return events.length > 0 ? events : null;
    } catch {
      return null;
    }
  }

  if (!content.startsWith("执行步骤\n")) {
    const legacyEvents = parseLegacyHermesExecutionEvents(content);
    return legacyEvents;
  }
  const events = content
    .split(/\r?\n/)
    .slice(1)
    .map((line) => normalizeExecutionEntry(line.trim().replace(/^- /, "")))
    .filter((event): event is ExecutionHistoryEntry => Boolean(event));
  return events.length > 0 ? events : null;
}

function parseLegacyHermesExecutionEvents(content: string): ExecutionHistoryEntry[] | null {
  const lines = content.split(/\r?\n/);
  const events: ExecutionHistoryEntry[] = [];

  for (let index = 0; index < lines.length; ) {
    const current = lines[index]?.trim() ?? "";
    if (!current) {
      index += 1;
      continue;
    }

    const match = current.match(LEGACY_HERMES_EXECUTION_LINE);
    if (!match) {
      index += 1;
      continue;
    }

    const tool = normalizeExecutionText(match[1]);
    const nextLine = lines[index + 1]?.trim() ?? "";
    const nextLooksLikeTool = LEGACY_HERMES_EXECUTION_LINE.test(nextLine);
    const detail = !nextLooksLikeTool && nextLine ? nextLine : match[2];
    const event = normalizeExecutionEntry({
      kind: "tool.call",
      tool,
      detail,
    });
    if (event) {
      events.push(event);
    }

    index += !nextLooksLikeTool && nextLine ? 2 : 1;
  }

  return events.length > 0 ? events : null;
}

function normalizeStoredExecutionEntry(value: unknown): ExecutionHistoryEntry | null {
  if (typeof value === "string") {
    return normalizeExecutionEntry(value);
  }
  if (!value || typeof value !== "object" || !("kind" in value)) {
    return null;
  }
  const event = value as HermesVerboseEvent;
  return normalizeExecutionEntry(event);
}

function ImagePreviewDialog({
  attachment,
  onClose,
  t,
}: {
  attachment: HermesAttachment;
  onClose: () => void;
  t: Translate;
}) {
  const imageSrc = attachment.data_url ?? attachment.download_url;

  useEffect(() => {
    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        onClose();
      }
    }

    // 预览层是临时界面状态，支持 Escape 关闭可以减少键盘用户的操作成本。
    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [onClose]);

  if (!imageSrc) {
    return null;
  }

  return (
    <div className="image-lightbox" role="dialog" aria-modal="true" aria-label={t("chat.imagePreview")}>
      <button
        type="button"
        className="image-lightbox-backdrop"
        aria-label={t("chat.previewBackdrop")}
        onClick={onClose}
      />
      <div className="image-lightbox-panel">
        <div className="image-lightbox-toolbar">
          <strong>{attachment.name}</strong>
          <button
            type="button"
            className="image-lightbox-close"
            aria-label={t("chat.previewClose")}
            onClick={onClose}
          >
            <X aria-hidden="true" size={18} />
          </button>
        </div>
        <img src={imageSrc} alt={attachment.name} />
      </div>
    </div>
  );
}
