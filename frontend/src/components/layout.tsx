import type { ReactNode } from "react";
import { Bot, Settings, Users } from "lucide-react";

type LayoutProps = {
  children: ReactNode;
};

export function Layout({ children }: LayoutProps) {
  return (
    <div className="shell">
      <aside className="sidebar" aria-label="Primary">
        <div className="brand">
          <Bot aria-hidden="true" size={22} />
          <span>Hermes Hub</span>
        </div>
        <nav>
          <a href="#admin">
            <Users aria-hidden="true" size={18} />
            Admin
          </a>
          <a href="#workspace">
            <Settings aria-hidden="true" size={18} />
            Workspace
          </a>
        </nav>
      </aside>
      <main className="content">{children}</main>
    </div>
  );
}
