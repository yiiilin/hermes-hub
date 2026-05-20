import type {
  ApiClient,
  Channel,
  ChannelMessage,
  ChannelSession,
  HermesAttachment,
  HermesInstance,
} from "../api/client";
import { useChatSidebar } from "../components/layout";
import { Bot, FileText, Image, Paperclip, Plus, Send } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";

type ChannelSessionRouteProps = {
  apiClient: ApiClient;
};

type BrowserCrypto = {
  randomUUID?: () => string;
  getRandomValues?: <T extends Uint8Array>(array: T) => T;
};

export function ChannelSessionRoute({ apiClient }: ChannelSessionRouteProps) {
  const setChatSidebar = useChatSidebar();
  const [channel, setChannel] = useState<Channel | null>(null);
  const [sessions, setSessions] = useState<ChannelSession[]>([]);
  const [selectedSession, setSelectedSession] = useState<ChannelSession | null>(null);
  const [instance, setInstance] = useState<HermesInstance | null>(null);
  const [messages, setMessages] = useState<ChannelMessage[]>([]);
  const [prompt, setPrompt] = useState("");
  const [attachments, setAttachments] = useState<HermesAttachment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const messageListRef = useRef<HTMLDivElement | null>(null);

  async function refreshWorkspace() {
    setError(null);
    try {
      const [channels, nextInstance] = await Promise.all([
        apiClient.listChannels(),
        apiClient.workspaceStatus(),
      ]);
      const hubChannel = channels.find((item) => item.name === "hermes-hub") ?? channels[0];
      setChannel(hubChannel ?? null);
      setInstance(nextInstance);

      if (hubChannel) {
        const nextSessions = await apiClient.listSessions(hubChannel.id);
        setSessions(nextSessions);
        const nextSelected =
          nextSessions.find((session) => session.id === selectedSession?.id) ??
          nextSessions[0] ??
          null;
        setSelectedSession(nextSelected);
        if (nextSelected) {
          setMessages(await apiClient.listSessionMessages(hubChannel.id, nextSelected.id));
        } else {
          setMessages([]);
        }
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Workspace data could not be loaded");
    }
  }

  useEffect(() => {
    void refreshWorkspace();
  }, []);

  useEffect(() => {
    const node = messageListRef.current;
    if (node) {
      node.scrollTop = node.scrollHeight;
    }
  }, [messages]);

  useEffect(() => {
    setChatSidebar?.(
      <ChatSidebar
        channel={channel}
        instance={instance}
        sessions={sessions}
        selectedSession={selectedSession}
        onCreate={() => void createSession()}
        onSelect={(session) => void selectSession(session)}
      />,
    );

    return () => setChatSidebar?.(null);
  }, [channel, instance, sessions, selectedSession, setChatSidebar]);

  async function createSession() {
    if (!channel) {
      return null;
    }
    const session = await apiClient.createSession(channel.id, "agent");
    setSessions((current) => [session, ...current]);
    setSelectedSession(session);
    setMessages([]);
    return session;
  }

  async function selectSession(session: ChannelSession) {
    if (!channel) {
      return;
    }
    setSelectedSession(session);
    setMessages(await apiClient.listSessionMessages(channel.id, session.id));
  }

  async function sendPrompt(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (!channel || (!prompt.trim() && attachments.length === 0)) {
      return;
    }

    setBusy(true);
    setError(null);

    const text = prompt.trim();
    const nextAttachments = attachments;
    setPrompt("");
    setAttachments([]);

    try {
      const session = selectedSession ?? (await createSession());
      if (!session) {
        throw new Error("Session could not be created");
      }

      const userMessage = await apiClient.appendSessionMessage(channel.id, session.id, {
        role: "user",
        content: text,
        attachments: nextAttachments,
      });
      setMessages((current) => [...current, userMessage]);

      if (!instance || instance.status !== "running") {
        setInstance(await apiClient.ensureHermes());
      }

      const assistantMessageId = createClientMessageId();
      let assistantContent = "";
      setMessages((current) => [
        ...current,
        {
          id: assistantMessageId,
          session_id: session.id,
          role: "assistant",
          content: "",
          attachments: [],
          created_at: Date.now(),
        },
      ]);
      const response = await apiClient.sendHermesPrompt(text, nextAttachments, session.id, {
        onDelta(delta) {
          assistantContent += delta;
          setMessages((current) =>
            current.map((message) =>
              message.id === assistantMessageId
                ? { ...message, content: message.content + delta }
                : message,
            ),
          );
        },
        onOutput(output) {
          assistantContent = output;
          setMessages((current) =>
            current.map((message) =>
              message.id === assistantMessageId ? { ...message, content: output } : message,
            ),
          );
        },
      });
      const finalAssistantContent =
        response || assistantContent || "Hermes returned an empty response.";
      const assistantMessage = await apiClient.appendSessionMessage(channel.id, session.id, {
        role: "assistant",
        content: finalAssistantContent,
        attachments: [],
      });
      setMessages((current) =>
        current.map((message) =>
          message.id === assistantMessageId ? assistantMessage : message,
        ),
      );
      if (text && !session.title) {
        const titled = await apiClient.generateSessionTitle(channel.id, session.id, text);
        setSelectedSession(titled);
        setSessions((current) =>
          current.map((item) => (item.id === titled.id ? titled : item)),
        );
      }
      await refreshWorkspace();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Hermes request failed");
    } finally {
      setBusy(false);
    }
  }

  async function pickFiles(files: FileList | null) {
    if (!files?.length) {
      return;
    }
    if (!channel) {
      return;
    }
    setError(null);
    try {
      const session = selectedSession ?? (await createSession());
      if (!session) {
        throw new Error("Session could not be created");
      }
      const selected = await apiClient.uploadSessionAttachments(channel.id, session.id, Array.from(files));
      setAttachments((current) => [...current, ...selected]);
      if (fileInputRef.current) {
        fileInputRef.current.value = "";
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Attachment upload failed");
    }
  }

  return (
    <section className="chat-workspace">
      <main className="chat-panel" aria-labelledby="chat-title">
        <header className="chat-header">
          <div>
            <span className="eyebrow">Hermes session</span>
            <h2 id="chat-title">{selectedSession?.title ?? "New conversation"}</h2>
          </div>
          <button type="button" className="secondary" onClick={() => void refreshWorkspace()}>
            Refresh
          </button>
        </header>

        <div className="message-list" ref={messageListRef}>
          {messages.length === 0 ? (
            <div className="empty-chat">
              <Bot aria-hidden="true" size={30} />
              <strong>Start a Hermes conversation</strong>
            </div>
          ) : (
            messages.map((message) => <MessageBubble key={message.id} message={message} />)
          )}
        </div>

        <form className="composer" onSubmit={sendPrompt}>
          {error ? <p className="error">{error}</p> : null}
          {attachments.length > 0 ? (
            <div className="attachment-row">
              {attachments.map((attachment) => (
                <span key={`${attachment.id ?? attachment.name}-${attachment.size ?? 0}`}>
                  {attachment.kind === "image" ? <Image size={15} /> : <FileText size={15} />}
                  {attachment.name}
                </span>
              ))}
            </div>
          ) : null}
          <textarea
            aria-label="Message"
            value={prompt}
            onChange={(event) => setPrompt(event.target.value)}
            placeholder="Message Hermes"
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
              className="secondary icon-text"
              onClick={() => fileInputRef.current?.click()}
            >
              <Paperclip aria-hidden="true" size={17} />
              Attach
            </button>
            <button type="submit" disabled={busy || (!prompt.trim() && attachments.length === 0)}>
              <Send aria-hidden="true" size={17} />
              Send
            </button>
          </div>
        </form>
      </main>
    </section>
  );
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
  channel,
  instance,
  sessions,
  selectedSession,
  onCreate,
  onSelect,
}: {
  channel: Channel | null;
  instance: HermesInstance | null;
  sessions: ChannelSession[];
  selectedSession: ChannelSession | null;
  onCreate: () => void;
  onSelect: (session: ChannelSession) => void;
}) {
  return (
    <div className="chat-sidebar-menu">
      <div>
        <span className="eyebrow">Channel</span>
        <h1>{channel?.name ?? "hermes-hub"}</h1>
        <p>{instance?.status ?? "Hermes not provisioned"}</p>
      </div>
      <button type="button" onClick={onCreate}>
        <Plus aria-hidden="true" size={17} />
        New chat
      </button>
      <ul className="session-list">
        {sessions.map((session) => (
          <li key={session.id}>
            <button
              type="button"
              className={selectedSession?.id === session.id ? "active" : ""}
              onClick={() => onSelect(session)}
            >
              {session.title ?? "New conversation"}
            </button>
          </li>
        ))}
      </ul>
    </div>
  );
}

function MessageBubble({ message }: { message: ChannelMessage }) {
  return (
    <article className={`message-bubble ${message.role}`}>
      <span>{message.role === "user" ? "You" : "Hermes"}</span>
      {message.attachments?.length ? (
        <div className="message-attachments">
          {message.attachments.map((attachment) => {
            const imageSrc = attachment.data_url ?? attachment.download_url;
            return attachment.kind === "image" && imageSrc ? (
              <img key={attachment.id ?? imageSrc} src={imageSrc} alt={attachment.name} />
            ) : (
              <a
                key={attachment.id ?? attachment.name}
                className="file-chip"
                href={attachment.download_url}
              >
                <FileText aria-hidden="true" size={16} />
                {attachment.name}
              </a>
            );
          })}
        </div>
      ) : null}
      <pre>{message.content}</pre>
    </article>
  );
}
