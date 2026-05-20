import type {
  ApiClient,
  Channel,
  ChannelSession,
  HermesAttachment,
  HermesInstance,
} from "../api/client";
import { Bot, FileText, Image, Paperclip, Plus, Send } from "lucide-react";
import { FormEvent, useEffect, useRef, useState } from "react";

type ChannelSessionRouteProps = {
  apiClient: ApiClient;
};

type ChatMessage = {
  id: string;
  role: "user" | "assistant";
  content: string;
  attachments?: HermesAttachment[];
};

type BrowserCrypto = {
  randomUUID?: () => string;
  getRandomValues?: <T extends Uint8Array>(array: T) => T;
};

export function ChannelSessionRoute({ apiClient }: ChannelSessionRouteProps) {
  const [channel, setChannel] = useState<Channel | null>(null);
  const [sessions, setSessions] = useState<ChannelSession[]>([]);
  const [selectedSession, setSelectedSession] = useState<ChannelSession | null>(null);
  const [instance, setInstance] = useState<HermesInstance | null>(null);
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [prompt, setPrompt] = useState("");
  const [attachments, setAttachments] = useState<HermesAttachment[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fileInputRef = useRef<HTMLInputElement | null>(null);

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
        setSelectedSession((current) => current ?? nextSessions[0] ?? null);
      }
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "Workspace data could not be loaded");
    }
  }

  useEffect(() => {
    void refreshWorkspace();
  }, []);

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

      setMessages((current) => [
        ...current,
        {
          id: createClientMessageId(),
          role: "user",
          content: text,
          attachments: nextAttachments,
        },
      ]);

      if (!instance || instance.status !== "running") {
        setInstance(await apiClient.ensureHermes());
      }

      const response = await apiClient.sendHermesPrompt(text, nextAttachments, session.id);
      setMessages((current) => [
        ...current,
        {
          id: createClientMessageId(),
          role: "assistant",
          content: response || "Hermes returned an empty response.",
        },
      ]);
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
    const selected = await Promise.all(Array.from(files).map(fileToAttachment));
    setAttachments((current) => [...current, ...selected]);
    if (fileInputRef.current) {
      fileInputRef.current.value = "";
    }
  }

  return (
    <section className="chat-workspace">
      <aside className="session-sidebar">
        <div>
          <span className="eyebrow">Channel</span>
          <h1>{channel?.name ?? "hermes-hub"}</h1>
          <p>{instance?.status ?? "Hermes not provisioned"}</p>
        </div>
        <button type="button" onClick={() => void createSession()}>
          <Plus aria-hidden="true" size={17} />
          New chat
        </button>
        <ul className="session-list">
          {sessions.map((session) => (
            <li key={session.id}>
              <button
                type="button"
                className={selectedSession?.id === session.id ? "active" : ""}
                onClick={() => {
                  setSelectedSession(session);
                  setMessages([]);
                }}
              >
                {session.title ?? "New conversation"}
              </button>
            </li>
          ))}
        </ul>
      </aside>

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

        <div className="message-list">
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
                <span key={`${attachment.name}-${attachment.data_url.length}`}>
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

function MessageBubble({ message }: { message: ChatMessage }) {
  return (
    <article className={`message-bubble ${message.role}`}>
      <span>{message.role === "user" ? "You" : "Hermes"}</span>
      {message.attachments?.length ? (
        <div className="message-attachments">
          {message.attachments.map((attachment) =>
            attachment.kind === "image" ? (
              <img key={attachment.data_url} src={attachment.data_url} alt={attachment.name} />
            ) : (
              <div key={attachment.data_url} className="file-chip">
                <FileText aria-hidden="true" size={16} />
                {attachment.name}
              </div>
            ),
          )}
        </div>
      ) : null}
      <pre>{message.content}</pre>
    </article>
  );
}

function fileToAttachment(file: File): Promise<HermesAttachment> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.addEventListener("load", () => {
      resolve({
        name: file.name,
        content_type: file.type || "application/octet-stream",
        data_url: String(reader.result),
        kind: file.type.startsWith("image/") ? "image" : "file",
      });
    });
    reader.addEventListener("error", () => reject(reader.error));
    reader.readAsDataURL(file);
  });
}
