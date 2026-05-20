import type { ReactNode } from "react";
import type { User } from "../api/client";
import { Bot, Cpu, LogOut, MessageSquare, Settings, Users } from "lucide-react";

export type AppView = "chat" | "admin-users" | "admin-models" | "admin-hermes";

type LayoutProps = {
  children: ReactNode;
  user: User | null;
  activeView: AppView;
  onNavigate: (view: AppView) => void;
  onLogout?: () => void;
};

export function Layout({ children, user, activeView, onNavigate, onLogout }: LayoutProps) {
  return (
    <div className="shell">
      <aside className="sidebar" aria-label="Primary">
        <div className="brand">
          <Bot aria-hidden="true" size={22} />
          <span>Hermes Hub</span>
        </div>
        <nav>
          {user ? (
            <button
              type="button"
              className={activeView === "chat" ? "nav-link active" : "nav-link"}
              onClick={() => onNavigate("chat")}
            >
              <MessageSquare aria-hidden="true" size={18} />
              对话
            </button>
          ) : null}
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
        </nav>
        {user ? (
          <div className="account">
            <span>{user.email}</span>
            <button type="button" className="icon-button" onClick={onLogout} aria-label="Sign out">
              <LogOut aria-hidden="true" size={17} />
            </button>
          </div>
        ) : null}
      </aside>
      <main className="content">{children}</main>
    </div>
  );
}
