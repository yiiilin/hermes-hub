import type { Dispatch, ReactNode, SetStateAction } from "react";
import type { User } from "../api/client";
import { Bot, Cpu, LogOut, Settings, Users } from "lucide-react";
import { createContext, useContext, useState } from "react";

export type AppView = "chat" | "admin-users" | "admin-models" | "admin-hermes";

type ChatSidebarSetter = Dispatch<SetStateAction<ReactNode>>;

const ChatSidebarContext = createContext<ChatSidebarSetter | null>(null);

type LayoutProps = {
  children: ReactNode;
  user: User | null;
  activeView: AppView;
  onNavigate: (view: AppView) => void;
  onLogout?: () => void;
};

export function Layout({ children, user, activeView, onNavigate, onLogout }: LayoutProps) {
  const [chatSidebar, setChatSidebar] = useState<ReactNode>(null);

  return (
    <ChatSidebarContext.Provider value={setChatSidebar}>
      <div className="shell">
        <aside className="sidebar" aria-label="Primary">
          <button type="button" className="brand brand-button" onClick={() => onNavigate("chat")}>
            <Bot aria-hidden="true" size={22} />
            <span>Hermes Hub</span>
          </button>
          {user ? <div className="sidebar-chat">{chatSidebar}</div> : null}
          <nav className="sidebar-bottom">
          {user?.role === "admin" ? (
            <div className="nav-group">
              <span className="nav-label">管理</span>
              <button
                type="button"
                className={activeView === "admin-users" ? "nav-link active" : "nav-link"}
                onClick={() => onNavigate("admin-users")}
              >
                <Users aria-hidden="true" size={18} />
                用户管理
              </button>
              <button
                type="button"
                className={activeView === "admin-models" ? "nav-link active" : "nav-link"}
                onClick={() => onNavigate("admin-models")}
              >
                <Settings aria-hidden="true" size={18} />
                模型配置管理
              </button>
              <button
                type="button"
                className={activeView === "admin-hermes" ? "nav-link active" : "nav-link"}
                onClick={() => onNavigate("admin-hermes")}
              >
                <Cpu aria-hidden="true" size={18} />
                Hermes 管理
              </button>
            </div>
          ) : null}
          {user ? (
            <div className="account">
              <span>{user.email}</span>
              <button type="button" className="icon-button" onClick={onLogout} aria-label="Sign out">
                <LogOut aria-hidden="true" size={17} />
              </button>
            </div>
          ) : null}
        </nav>
        </aside>
        <main className={activeView === "chat" ? "content chat-content" : "content"}>
          {children}
        </main>
      </div>
    </ChatSidebarContext.Provider>
  );
}

export function useChatSidebar() {
  return useContext(ChatSidebarContext);
}
