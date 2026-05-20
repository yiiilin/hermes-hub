import type { ReactNode } from "react";
import type { User } from "../api/client";
import { Bot, LogOut, Settings, Users } from "lucide-react";

type LayoutProps = {
  children: ReactNode;
  user: User | null;
  onLogout?: () => void;
};

export function Layout({ children, user, onLogout }: LayoutProps) {
  return (
    <div className="shell">
      <aside className="sidebar" aria-label="Primary">
        <div className="brand">
          <Bot aria-hidden="true" size={22} />
          <span>Hermes Hub</span>
        </div>
        <nav>
          {user?.role === "admin" ? (
            <a href="#admin">
              <Users aria-hidden="true" size={18} />
              Admin
            </a>
          ) : null}
          {user ? (
            <a href="#workspace">
              <Settings aria-hidden="true" size={18} />
              Workspace
            </a>
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
