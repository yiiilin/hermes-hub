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
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
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
import {
  Children,
  ClipboardEvent as ReactClipboardEvent,
  FormEvent,
  ReactNode,
  memo,
  useCallback,
  useEffect,
  isValidElement,
  useMemo,
  useRef,
  useState,
} from "react";

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
type Language = ReturnType<typeof useI18n>["language"];
type ExecutionHistoryEntry = HermesVerboseEvent;
type RenderableMessage = {
  message: ChannelMessage;
  pending: boolean;
};

const MESSAGE_VIRTUALIZATION_THRESHOLD = 80;
const MESSAGE_VIRTUALIZATION_OVERSCAN_PX = 900;
const MESSAGE_VIRTUALIZATION_DEFAULT_VIEWPORT_PX = 720;
const MESSAGE_VIRTUALIZATION_DEFAULT_ROW_HEIGHT_PX = 112;
const MESSAGE_VIRTUALIZATION_MIN_ROW_HEIGHT_PX = 44;
const MESSAGE_VIRTUALIZATION_DEFAULT_GAP_PX = 16;

export function ChannelSessionRoute({
  active = true,
  apiClient,
  onOpenChat,
}: ChannelSessionRouteProps) {
  const { t, language } = useI18n();
  const setChatSidebar = useChatSidebar();
  const sidebarCollapsed = useSidebarCollapsed();
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [selectedSession, setSelectedSession] = useState<SessionSummary | null>(null);
  const [seenSessionUpdates, setSeenSessionUpdates] = useState<Record<string, number>>({});
  const [unreadSessionIds, setUnreadSessionIds] = useState<Set<string>>(() => new Set());
  const [messages, setMessages] = useState<ChannelMessage[]>([]);
  const [previewAttachment, setPreviewAttachment] = useState<HermesAttachment | null>(null);
  const [pendingAssistantMessageId, setPendingAssistantMessageId] = useState<string | null>(null);
  const [pendingAssistantSessionId, setPendingAssistantSessionId] = useState<string | null>(null);
  const [activeRun, setActiveRun] = useState<HermesActiveRun | null>(null);
  const [verboseEvents, setVerboseEvents] = useState<ExecutionHistoryEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
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
          if (isExecutionHistoryMessage(message)) {
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

  async function submitPrompt(text: string, nextAttachments: HermesAttachment[]) {
    if (!selectedSession && (!text && nextAttachments.length === 0)) {
      return;
    }

    setBusy(true);
    setError(null);

    resetVerboseEvents();
    stickToBottomRef.current = true;
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

  async function uploadAttachmentFiles(files: File[]) {
    if (files.length === 0) {
      return [];
    }
    setError(null);
    try {
      const session = selectedSession ?? (await createSession());
      if (!session) {
        throw new Error(t("chat.sessionCreateFailed"));
      }
      return await apiClient.uploadSessionAttachmentsPublic(session.id, files);
    } catch (cause) {
      setError(userFacingErrorMessage(cause, t("chat.attachmentUploadFailed"), t));
      return [];
    }
  }

  // 会话切换和 SSE snapshot 都是异步状态更新；渲染层再按当前会话兜底过滤，
  // 避免旧会话消息在新会话页面短暂或异常残留。
  const selectedSessionId = selectedSession?.id ?? null;
  const renderedMessages = useMemo(
    () =>
      sortMessagesForDisplay(
        messages.filter((message) => !selectedSessionId || message.session_id === selectedSessionId),
      ),
    [messages, selectedSessionId],
  );
  const runInProgress = Boolean(activeRun && !isTerminalHermesRun(activeRun));
  const liveExecutionVisible = Boolean(
    pendingAssistantSessionId === selectedSession?.id && verboseEvents.length > 0,
  );
  const activeExecutionMessage = useMemo(
    () => activeExecutionMessageForRun(renderedMessages, activeRun),
    [activeRun, renderedMessages],
  );

  if (!active) {
    return null;
  }

  return (
    <section className="chat-workspace">
      <main className="chat-panel" aria-labelledby="chat-title">
        <header className="chat-header">
          <div className="chat-title-row">
            <h2 id="chat-title">{selectedSession?.title ?? t("chat.newConversation")}</h2>
            {runInProgress ? (
              <span className="header-typing" aria-live="polite">
                {t("chat.typing")}
              </span>
            ) : null}
          </div>
          <button type="button" className="secondary" onClick={() => void refreshSessions()}>
            <RefreshCw aria-hidden="true" size={16} />
            {t("chat.refresh")}
          </button>
        </header>

        <MessageList
          activeExecutionMessageId={activeExecutionMessage?.id ?? null}
          language={language}
          liveExecutionVisible={liveExecutionVisible}
          messageListRef={messageListRef}
          messages={renderedMessages}
          onPreviewImage={setPreviewAttachment}
          pendingAssistantMessageId={pendingAssistantMessageId}
          stickToBottomRef={stickToBottomRef}
          t={t}
          verboseEvents={verboseEvents}
        />

        <ChatComposer
          busy={busy}
          error={error}
          runInProgress={runInProgress}
          onPreviewImage={setPreviewAttachment}
          onStop={() => void stopCurrentRun()}
          onSubmit={submitPrompt}
          onUploadAttachments={uploadAttachmentFiles}
          t={t}
        />
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
const LEGACY_HERMES_EXECUTION_HINT = /(^|\n)\S+\s+[A-Za-z0-9_.-]+\(/u;
const EXECUTION_HISTORY_EVENTS_CACHE_LIMIT = 500;
const executionHistoryEventsCache = new Map<string, ExecutionHistoryEntry[] | null>();
const messageTimeFormatters = new Map<Language, Intl.DateTimeFormat>();

function filesFromClipboardData(clipboardData: DataTransfer) {
  const directFiles = Array.from(clipboardData.files ?? []);
  if (directFiles.length > 0) {
    return directFiles;
  }

  return Array.from(clipboardData.items ?? []).flatMap((item) => {
    if (item.kind !== "file") {
      return [];
    }
    const file = item.getAsFile();
    return file ? [file] : [];
  });
}

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
    // 程序滚动到底部时同步触发滚动监听，让虚拟列表和贴底状态立刻读到新位置。
    node.dispatchEvent(new Event("scroll"));
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

const MessageList = memo(function MessageList({
  activeExecutionMessageId,
  language,
  liveExecutionVisible,
  messageListRef,
  messages,
  onPreviewImage,
  pendingAssistantMessageId,
  stickToBottomRef,
  t,
  verboseEvents,
}: {
  activeExecutionMessageId: string | null;
  language: Language;
  liveExecutionVisible: boolean;
  messageListRef: { current: HTMLDivElement | null };
  messages: ChannelMessage[];
  onPreviewImage: (attachment: HermesAttachment) => void;
  pendingAssistantMessageId: string | null;
  stickToBottomRef: { current: boolean };
  t: Translate;
  verboseEvents: ExecutionHistoryEntry[];
}) {
  const renderableMessages = useMemo(
    () =>
      messages.reduce<RenderableMessage[]>((current, message) => {
        const pending = message.id === pendingAssistantMessageId;
        if (liveExecutionVisible && activeExecutionMessageId === message.id && !pending) {
          return current;
        }
        if (!shouldRenderMessageBubble(message, pending)) {
          return current;
        }
        current.push({ message, pending });
        return current;
      }, []),
    [activeExecutionMessageId, liveExecutionVisible, messages, pendingAssistantMessageId],
  );
  const virtualized = renderableMessages.length > MESSAGE_VIRTUALIZATION_THRESHOLD;
  const virtualWindow = useVirtualMessageWindow(
    renderableMessages,
    virtualized,
    messageListRef,
    stickToBottomRef,
  );

  function renderMessage(entry: RenderableMessage) {
    return (
      <MessageBubble
        message={entry.message}
        pending={entry.pending}
        executionEvents={
          entry.pending && verboseEvents.length > 0 ? verboseEvents : undefined
        }
        onPreviewImage={onPreviewImage}
        t={t}
        language={language}
      />
    );
  }

  return (
    <div
      className={[
        messages.length === 0 ? "message-list empty" : "message-list",
        virtualized ? "virtualized" : "",
      ]
        .filter(Boolean)
        .join(" ")}
      ref={messageListRef}
    >
      {messages.length === 0 ? (
        <div className="empty-chat">
          <Bot aria-hidden="true" size={30} />
          <strong>{t("chat.empty")}</strong>
        </div>
      ) : virtualized ? (
        <div
          className="message-virtual-spacer"
          style={{ height: `${virtualWindow.totalHeight}px` }}
        >
          {virtualWindow.items.map((entry, offset) => {
            const index = virtualWindow.startIndex + offset;
            return (
              <VirtualMessageRow
                key={entry.message.id}
                messageId={entry.message.id}
                measureKey={virtualMessageMeasureKey(entry, verboseEvents)}
                top={virtualWindow.offsets[index] ?? 0}
                onMeasure={virtualWindow.measureRow}
              >
                {renderMessage(entry)}
              </VirtualMessageRow>
            );
          })}
        </div>
      ) : (
        renderableMessages.map((entry) => (
          <MessageBubble
            key={entry.message.id}
            message={entry.message}
            pending={entry.pending}
            executionEvents={
              entry.pending && verboseEvents.length > 0 ? verboseEvents : undefined
            }
            onPreviewImage={onPreviewImage}
            t={t}
            language={language}
          />
        ))
      )}
    </div>
  );
});

function useVirtualMessageWindow(
  entries: RenderableMessage[],
  enabled: boolean,
  messageListRef: { current: HTMLDivElement | null },
  stickToBottomRef: { current: boolean },
) {
  const measuredHeightsRef = useRef<Record<string, number>>({});
  const [heightVersion, setHeightVersion] = useState(0);
  const [rowGap, setRowGap] = useState(MESSAGE_VIRTUALIZATION_DEFAULT_GAP_PX);
  const [viewport, setViewport] = useState({ scrollTop: 0, height: 0 });

  useEffect(() => {
    if (!enabled) {
      return;
    }

    const liveIds = new Set(entries.map((entry) => entry.message.id));
    let changed = false;
    for (const id of Object.keys(measuredHeightsRef.current)) {
      if (!liveIds.has(id)) {
        delete measuredHeightsRef.current[id];
        changed = true;
      }
    }
    if (changed) {
      setHeightVersion((version) => version + 1);
    }
  }, [enabled, entries]);

  useEffect(() => {
    if (!enabled) {
      return;
    }

    const node = messageListRef.current;
    if (!node) {
      return;
    }

    let animationFrame = 0;
    function updateViewport() {
      window.cancelAnimationFrame(animationFrame);
      animationFrame = window.requestAnimationFrame(() => {
        const current = messageListRef.current;
        if (!current) {
          return;
        }
        const nextGap = readMessageListGap(current);
        setRowGap((previous) => (Math.abs(previous - nextGap) < 0.5 ? previous : nextGap));
        setViewport((previous) => {
          const next = {
            scrollTop: current.scrollTop,
            height: current.clientHeight,
          };
          return previous.scrollTop === next.scrollTop && previous.height === next.height
            ? previous
            : next;
        });
      });
    }

    updateViewport();
    node.addEventListener("scroll", updateViewport, { passive: true });
    window.addEventListener("resize", updateViewport);
    const observer =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(updateViewport);
    observer?.observe(node);

    return () => {
      window.cancelAnimationFrame(animationFrame);
      node.removeEventListener("scroll", updateViewport);
      window.removeEventListener("resize", updateViewport);
      observer?.disconnect();
    };
  }, [enabled, entries.length, messageListRef]);

  const virtualMetrics = useMemo(() => {
    const heights = entries.map((entry) =>
      measuredHeightsRef.current[entry.message.id] ??
      estimateMessageRowHeight(entry.message),
    );
    const offsets: number[] = [];
    let totalHeight = 0;
    for (let index = 0; index < heights.length; index += 1) {
      offsets[index] = totalHeight;
      totalHeight += heights[index] + (index === heights.length - 1 ? 0 : rowGap);
    }

    return { heights, offsets, totalHeight };
  }, [entries, heightVersion, rowGap]);

  const layout = useMemo(() => {
    const { heights, offsets, totalHeight } = virtualMetrics;
    if (!enabled || entries.length === 0) {
      return {
        offsets,
        totalHeight,
        startIndex: 0,
        endIndex: entries.length,
        items: entries,
      };
    }

    const viewportHeight =
      viewport.height > 0 ? viewport.height : MESSAGE_VIRTUALIZATION_DEFAULT_VIEWPORT_PX;
    const preferredBottom =
      stickToBottomRef.current && viewport.scrollTop <= 0 && totalHeight > viewportHeight;
    const scrollTop = preferredBottom
      ? Math.max(0, totalHeight - viewportHeight)
      : viewport.scrollTop;
    const visibleStart = Math.max(0, scrollTop - MESSAGE_VIRTUALIZATION_OVERSCAN_PX);
    const visibleEnd = scrollTop + viewportHeight + MESSAGE_VIRTUALIZATION_OVERSCAN_PX;
    const startIndex = findFirstVisibleMessageIndex(offsets, heights, rowGap, visibleStart);
    const endIndex = findLastVisibleMessageIndex(offsets, visibleEnd, entries.length);

    return {
      offsets,
      totalHeight,
      startIndex,
      endIndex,
      items: entries.slice(startIndex, endIndex),
    };
  }, [enabled, entries, stickToBottomRef, viewport.height, viewport.scrollTop, virtualMetrics]);

  const measureRow = useCallback(
    (messageId: string, height: number) => {
      if (!Number.isFinite(height) || height <= 0) {
        return;
      }

      const normalizedHeight = Math.max(
        MESSAGE_VIRTUALIZATION_MIN_ROW_HEIGHT_PX,
        Math.ceil(height),
      );
      const previous = measuredHeightsRef.current[messageId];
      if (previous !== undefined && Math.abs(previous - normalizedHeight) < 1) {
        return;
      }

      const node = messageListRef.current;
      const shouldKeepBottom = Boolean(
        node && stickToBottomRef.current && isMessageListNearBottom(node),
      );
      measuredHeightsRef.current[messageId] = normalizedHeight;
      setHeightVersion((version) => version + 1);
      if (shouldKeepBottom) {
        scrollMessageListToBottom(node);
      }
    },
    [messageListRef, stickToBottomRef],
  );

  return { ...layout, measureRow };
}

function VirtualMessageRow({
  children,
  measureKey,
  messageId,
  onMeasure,
  top,
}: {
  children: ReactNode;
  measureKey: string;
  messageId: string;
  onMeasure: (messageId: string, height: number) => void;
  top: number;
}) {
  const rowRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const node = rowRef.current;
    if (!node) {
      return;
    }

    function measure() {
      const current = rowRef.current;
      if (!current) {
        return;
      }
      const height = current.getBoundingClientRect().height || current.offsetHeight;
      onMeasure(messageId, height);
    }

    measure();
    const observer = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(measure);
    observer?.observe(node);
    return () => observer?.disconnect();
  }, [measureKey, messageId, onMeasure]);

  return (
    <div
      className="message-virtual-row"
      ref={rowRef}
      style={{ transform: `translateY(${top}px)` }}
    >
      {children}
    </div>
  );
}

function virtualMessageMeasureKey(entry: RenderableMessage, verboseEvents: ExecutionHistoryEntry[]) {
  return [
    entry.message.id,
    entry.message.updated_at ?? entry.message.created_at,
    entry.message.content.length,
    entry.message.attachments?.length ?? 0,
    entry.pending ? "pending" : "ready",
    entry.pending ? verboseEvents.length : 0,
  ].join(":");
}

function estimateMessageRowHeight(message: ChannelMessage) {
  const lineCount = Math.max(1, message.content.split("\n").length);
  const contentBlocks = Math.ceil(message.content.length / 96);
  const attachmentHeight = (message.attachments?.length ?? 0) * 56;
  const executionHeight = message.message_kind === "execution" ? 48 : 0;
  return Math.max(
    MESSAGE_VIRTUALIZATION_MIN_ROW_HEIGHT_PX,
    Math.min(
      360,
      MESSAGE_VIRTUALIZATION_DEFAULT_ROW_HEIGHT_PX +
        contentBlocks * 18 +
        (lineCount - 1) * 12 +
        attachmentHeight +
        executionHeight,
    ),
  );
}

function readMessageListGap(node: HTMLElement) {
  const styles = window.getComputedStyle(node);
  const rawGap =
    styles.getPropertyValue("--message-list-gap").trim() || styles.rowGap || styles.gap;
  const parsed = Number.parseFloat(rawGap);
  return Number.isFinite(parsed) && parsed > 0
    ? parsed
    : MESSAGE_VIRTUALIZATION_DEFAULT_GAP_PX;
}

function findFirstVisibleMessageIndex(
  offsets: number[],
  heights: number[],
  rowGap: number,
  visibleStart: number,
) {
  let low = 0;
  let high = offsets.length;
  while (low < high) {
    const mid = Math.floor((low + high) / 2);
    const rowBottom = offsets[mid] + heights[mid] + rowGap;
    if (rowBottom < visibleStart) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  return Math.max(0, Math.min(low, offsets.length - 1));
}

function findLastVisibleMessageIndex(
  offsets: number[],
  visibleEnd: number,
  itemCount: number,
) {
  let low = 0;
  let high = offsets.length;
  while (low < high) {
    const mid = Math.floor((low + high) / 2);
    if (offsets[mid] <= visibleEnd) {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  return Math.min(itemCount, Math.max(1, low + 1));
}

const ChatComposer = memo(function ChatComposer({
  busy,
  error,
  runInProgress,
  onPreviewImage,
  onStop,
  onSubmit,
  onUploadAttachments,
  t,
}: {
  busy: boolean;
  error: string | null;
  runInProgress: boolean;
  onPreviewImage: (attachment: HermesAttachment) => void;
  onStop: () => void;
  onSubmit: (text: string, attachments: HermesAttachment[]) => Promise<void>;
  onUploadAttachments: (files: File[]) => Promise<HermesAttachment[]>;
  t: Translate;
}) {
  const [prompt, setPrompt] = useState("");
  const [attachments, setAttachments] = useState<HermesAttachment[]>([]);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const composerInputRef = useRef<HTMLTextAreaElement | null>(null);

  function focusComposerInputSoon() {
    const schedule = globalThis.requestAnimationFrame ?? ((callback: FrameRequestCallback) => {
      globalThis.setTimeout(callback, 0);
      return 0;
    });
    schedule(() => composerInputRef.current?.focus());
  }

  async function submitComposer() {
    const text = prompt.trim();
    if (!text && attachments.length === 0) {
      return;
    }

    const nextAttachments = attachments;
    // 输入态只属于 Composer；先清空本地草稿，避免父级消息流变化牵动每次键入。
    setPrompt("");
    setAttachments([]);
    focusComposerInputSoon();
    await onSubmit(text, nextAttachments);
  }

  async function sendPrompt(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    await submitComposer();
  }

  async function uploadAttachmentFiles(files: File[]) {
    const selected = await onUploadAttachments(files);
    if (selected.length > 0) {
      setAttachments((current) => [...current, ...selected]);
    }
  }

  async function pickFiles(files: FileList | null) {
    await uploadAttachmentFiles(files ? Array.from(files) : []);
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
    }
  }

  function pasteFiles(event: ReactClipboardEvent<HTMLTextAreaElement>) {
    const files = filesFromClipboardData(event.clipboardData);
    if (files.length === 0) {
      return;
    }
    // 只要剪贴板里有文件，就按附件处理，避免图片被浏览器插入成不可控内容。
    event.preventDefault();
    void uploadAttachmentFiles(files);
  }

  return (
    <form className="composer" onSubmit={sendPrompt}>
      {error ? <p className="error">{error}</p> : null}
      {attachments.length > 0 ? (
        <div className="attachment-row">
          {attachments.map((attachment) => (
            <ComposerAttachmentChip
              key={`${attachment.id ?? attachment.name}-${attachment.size ?? 0}`}
              attachment={attachment}
              onPreviewImage={onPreviewImage}
              t={t}
            />
          ))}
        </div>
      ) : null}
      <textarea
        ref={composerInputRef}
        aria-label={t("chat.messageLabel")}
        value={prompt}
        onChange={(event) => setPrompt(event.target.value)}
        onPaste={pasteFiles}
        placeholder={t("chat.messagePlaceholder")}
        onKeyDown={(event) => {
          // 中文/日文等输入法候选确认时也会触发 Enter，必须让组合输入先完成。
          if (event.key !== "Enter" || event.shiftKey || event.nativeEvent.isComposing) {
            return;
          }
          if (!busy) {
            event.preventDefault();
            void submitComposer();
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
          onClick={onStop}
        >
          <CircleStop aria-hidden="true" size={17} />
          {t("chat.stop")}
        </button>
        <button type="submit" disabled={busy || (!prompt.trim() && attachments.length === 0)}>
          <Send aria-hidden="true" size={17} />
          {t("chat.send")}
        </button>
      </div>
    </form>
  );
});

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
      message.client_message_key === executionKey && isExecutionHistoryMessage(message),
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
          isExecutionHistoryMessage(message) &&
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
      isExecutionHistoryMessage(message),
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

const MessageBubble = memo(function MessageBubble({
  message,
  pending = false,
  executionEvents: executionEventsOverride,
  onPreviewImage,
  t,
  language,
}: {
  message: ChannelMessage;
  pending?: boolean;
  executionEvents?: ExecutionHistoryEntry[];
  onPreviewImage: (attachment: HermesAttachment) => void;
  t: Translate;
  language: Language;
}) {
  const executionEvents =
    executionEventsOverride ??
    (isExecutionHistoryMessage(message) ? executionHistoryEvents(message.content) : undefined);
  const hasExecutionEvents = Array.isArray(executionEvents) && executionEvents.length > 0;
  const hasVisibleBody = Boolean(
    message.content || message.attachments?.length || hasExecutionEvents || pending,
  );
  const attachments = message.attachments ?? [];
  const updatedAt = messageUpdatedAtDate(message);

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
      {updatedAt ? (
        <time
          className={[
            "message-time",
            message.role === "user" ? "message-time-end" : "message-time-start",
          ].join(" ")}
          dateTime={updatedAt.toISOString()}
          title={updatedAt.toLocaleString(language)}
        >
          {formatMessageUpdatedTime(updatedAt, language)}
        </time>
      ) : null}
    </article>
  );
});

function formatMessageUpdatedTime(date: Date, language: Language) {
  let formatter = messageTimeFormatters.get(language);
  if (!formatter) {
    formatter = new Intl.DateTimeFormat(language, {
      hour: "2-digit",
      minute: "2-digit",
    });
    messageTimeFormatters.set(language, formatter);
  }
  return formatter.format(date);
}

function messageUpdatedAtDate(message: ChannelMessage) {
  const timestamp = message.updated_at ?? message.created_at;
  if (!Number.isFinite(timestamp)) {
    return null;
  }

  // 后端返回秒级时间戳；本地 pending/mock 消息沿用 Date.now() 的毫秒值。
  return new Date(timestamp > 1_000_000_000_000 ? timestamp : timestamp * 1000);
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

      // 同一时间戳内保留 Hub/SSE 传来的追加顺序；执行步骤也是消息，不能被特殊排序挪动。
      return left.index - right.index;
    })
    .map(({ message }) => message);
  return dedupeRepeatedAssistantMessages(sorted);
}

function mergeExecutionHistoryMessages(messages: ChannelMessage[]) {
  // 执行块按消息逐条保留，不再把不同轮次的执行过程折叠成一条。
  return messages;
}

function isExecutionHistoryMessage(message: ChannelMessage) {
  if (message.message_kind) {
    return message.message_kind === "execution";
  }

  // 旧后端或测试数据没有 message_kind 时，继续按内容兜底识别历史执行步骤。
  return isExecutionHistoryContent(message.content);
}

function isExecutionHistoryContent(content: string) {
  if (content.startsWith(`${EXECUTION_HISTORY_MARKER}\n`) || content.startsWith("执行步骤\n")) {
    return true;
  }

  return mayContainLegacyHermesExecution(content) && Boolean(executionHistoryEvents(content));
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
    !isExecutionHistoryMessage(left) &&
    !isExecutionHistoryMessage(right) &&
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
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={markdownComponents(attachments, referencedAttachmentIds, onPreviewImage, t)}
      >
        {content}
      </ReactMarkdown>
      <InlineAttachments
        attachments={attachments}
        referencedAttachmentIds={referencedAttachmentIds}
        onPreviewImage={onPreviewImage}
        t={t}
      />
    </div>
  );
}

function markdownComponents(
  attachments: HermesAttachment[],
  referencedAttachmentIds: Set<string>,
  onPreviewImage: (attachment: HermesAttachment) => void,
  t: Translate,
): Components {
  return {
    a({ href, children }) {
      const url = href ?? "";
      const attachment = attachmentForUrl(attachments, url);
      if (attachment) {
        referencedAttachmentIds.add(attachment.id ?? attachment.download_url ?? attachment.name);
        return <InlineAttachment attachment={attachment} onPreviewImage={onPreviewImage} t={t} />;
      }
      const safeHref = safeMarkdownUrl(url, false);
      return safeHref ? (
        <a href={safeHref} rel="noreferrer" target="_blank">
          {children}
        </a>
      ) : (
        <>{children}</>
      );
    },
    img({ src, alt }) {
      const url = src ?? "";
      const attachment = attachmentForUrl(attachments, url);
      if (attachment) {
        referencedAttachmentIds.add(attachment.id ?? attachment.download_url ?? attachment.name);
        return (
          <InlineAttachment
            attachment={{ ...attachment, name: alt || attachment.name }}
            onPreviewImage={onPreviewImage}
            t={t}
          />
        );
      }
      const imageUrl = safeMarkdownUrl(url, true);
      if (imageUrl) {
        return (
          <button
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
            <img src={imageUrl} alt={alt || imageUrl} loading="lazy" decoding="async" />
          </button>
        );
      }
      return <>{alt}</>;
    },
    pre({ children }) {
      const language = markdownCodeLanguage(children);
      return (
        <div className="markdown-code-block">
          {language ? <span className="markdown-code-language">{language}</span> : null}
          <pre className="markdown-code">{children}</pre>
        </div>
      );
    },
    code({ className, children }) {
      return <code className={className}>{children}</code>;
    },
  };
}

function markdownCodeLanguage(children: ReactNode): string | null {
  const child = Children.toArray(children).find((item) => isValidElement(item));
  if (!child || !isValidElement<{ className?: string }>(child)) {
    return null;
  }
  return /(?:^|\s)language-(\S+)/.exec(child.props.className ?? "")?.[1] ?? null;
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
        <img src={imageSrc} alt={attachment.name} loading="lazy" decoding="async" />
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

function ComposerAttachmentChip({
  attachment,
  onPreviewImage,
  t,
}: {
  attachment: HermesAttachment;
  onPreviewImage: (attachment: HermesAttachment) => void;
  t: Translate;
}) {
  const imageSrc = attachment.data_url ?? attachment.download_url;
  const canPreviewImage =
    (attachment.kind === "image" || attachment.content_type.startsWith("image/")) && imageSrc;

  if (canPreviewImage) {
    return (
      <button
        type="button"
        className="composer-attachment-chip composer-attachment-image"
        aria-label={t("chat.markdownImage", { name: attachment.name })}
        onClick={() => onPreviewImage(attachment)}
      >
        <img src={imageSrc} alt="" aria-hidden="true" loading="lazy" decoding="async" />
        <span>{attachment.name}</span>
      </button>
    );
  }

  return (
    <span className="composer-attachment-chip">
      {attachmentIcon(attachment, 15)}
      <span>{attachment.name}</span>
    </span>
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
  if (executionHistoryEventsCache.has(content)) {
    return executionHistoryEventsCache.get(content) ?? null;
  }

  const events = parseExecutionHistoryEventsUncached(content);
  rememberExecutionHistoryEvents(content, events);
  return events;
}

function parseExecutionHistoryEventsUncached(content: string): ExecutionHistoryEntry[] | null {
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
    if (!mayContainLegacyHermesExecution(content)) {
      return null;
    }
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

function rememberExecutionHistoryEvents(content: string, events: ExecutionHistoryEntry[] | null) {
  // 消息渲染会多次判断同一段 Markdown 是否是执行步骤；缓存正负结果，避免长文本反复 split/regex。
  if (executionHistoryEventsCache.size >= EXECUTION_HISTORY_EVENTS_CACHE_LIMIT) {
    const oldestKey = executionHistoryEventsCache.keys().next().value;
    if (oldestKey !== undefined) {
      executionHistoryEventsCache.delete(oldestKey);
    }
  }
  executionHistoryEventsCache.set(content, events);
}

function mayContainLegacyHermesExecution(content: string) {
  return content.includes("(") && LEGACY_HERMES_EXECUTION_HINT.test(content);
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
