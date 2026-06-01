import type { Dispatch, MouseEvent, ReactNode, SetStateAction } from "react";
import type { User } from "../api/client";
import {
  Bot,
  CalendarClock,
  LogIn,
  LogOut,
  Menu,
  PanelLeftClose,
  PanelLeftOpen,
  SlidersHorizontal,
  UserCog,
  X,
} from "lucide-react";
import { useI18n } from "../i18n";
import { createContext, useContext, useEffect, useState } from "react";

export type AppView = "chat" | "login" | "admin-settings" | "scheduled-tasks" | "personal-settings";

type ChatSidebarSetter = Dispatch<SetStateAction<ReactNode>>;

const ChatSidebarContext = createContext<ChatSidebarSetter | null>(null);
const SidebarCollapsedContext = createContext(false);

type LayoutProps = {
  children: ReactNode;
  user: User | null;
  activeView: AppView;
  onNavigate: (view: AppView) => void;
  onLogin?: () => void;
  onLogout?: () => void;
};

export function Layout({ children, user, activeView, onNavigate, onLogin, onLogout }: LayoutProps) {
  const { t } = useI18n();
  const [chatSidebar, setChatSidebar] = useState<ReactNode>(null);
  const [mobileSidebarOpen, setMobileSidebarOpen] = useState(false);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);

  useEffect(() => {
    if (!mobileSidebarOpen) {
      return;
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setMobileSidebarOpen(false);
      }
    }

    window.addEventListener("keydown", closeOnEscape);
    return () => window.removeEventListener("keydown", closeOnEscape);
  }, [mobileSidebarOpen]);

  function navigate(view: AppView) {
    onNavigate(view);
    setMobileSidebarOpen(false);
  }

  async function logout() {
    setMobileSidebarOpen(false);
    await onLogout?.();
  }

  function closeMobileSidebarAfterAction(event: MouseEvent<HTMLElement>) {
    if (!mobileSidebarOpen) {
      return;
    }

    if ((event.target as HTMLElement).closest(".keep-sidebar-open")) {
      return;
    }

    // 移动端抽屉里的会话、管理菜单、退出等动作完成后应回到主内容区。
    if ((event.target as HTMLElement).closest("button")) {
      setMobileSidebarOpen(false);
    }
  }

  return (
    <ChatSidebarContext.Provider value={setChatSidebar}>
      <SidebarCollapsedContext.Provider value={sidebarCollapsed}>
        <div className={sidebarCollapsed ? "shell sidebar-collapsed" : "shell"}>
        <header className="mobile-topbar">
          <button
            type="button"
            className="icon-button mobile-menu-button"
            aria-label={t("layout.openMenu")}
            aria-expanded={mobileSidebarOpen}
            onClick={() => setMobileSidebarOpen(true)}
          >
            <Menu aria-hidden="true" size={19} />
          </button>
          <button type="button" className="mobile-brand" onClick={() => navigate("chat")}>
            <Bot aria-hidden="true" size={18} />
            <span>Hermes Hub</span>
          </button>
        </header>
        {mobileSidebarOpen ? (
          <button
            type="button"
            className="sidebar-backdrop"
            aria-label={t("layout.closeMenu")}
            onClick={() => setMobileSidebarOpen(false)}
          />
        ) : null}
        <aside
          className={mobileSidebarOpen ? "sidebar open" : "sidebar"}
          aria-label={t("layout.primary")}
          onClickCapture={closeMobileSidebarAfterAction}
        >
          <div className="sidebar-heading">
            <button type="button" className="brand brand-button" onClick={() => navigate("chat")}>
              <Bot aria-hidden="true" size={22} />
              <span>Hermes Hub</span>
            </button>
            <button
              type="button"
              className="icon-button drawer-close"
              aria-label={t("layout.closeMenu")}
              onClick={() => setMobileSidebarOpen(false)}
            >
              <X aria-hidden="true" size={18} />
            </button>
          </div>
          <div className="sidebar-desktop-header">
            <button
              type="button"
              className="brand brand-button desktop-brand"
              onClick={() => navigate("chat")}
            >
              <Bot aria-hidden="true" size={22} />
              <span>Hermes Hub</span>
            </button>
            <button
              type="button"
              className="icon-button desktop-sidebar-toggle"
              aria-label={
                sidebarCollapsed ? t("layout.expandSidebar") : t("layout.collapseSidebar")
              }
              onClick={() => setSidebarCollapsed((collapsed) => !collapsed)}
            >
              {sidebarCollapsed ? (
                <PanelLeftOpen aria-hidden="true" size={18} />
              ) : (
                <PanelLeftClose aria-hidden="true" size={18} />
              )}
            </button>
          </div>
          <div className="sidebar-chat">{chatSidebar}</div>
          <nav className="sidebar-bottom">
            {user?.role === "admin" ? (
              <div className="nav-group">
                <button
                  type="button"
                  className={activeView === "admin-settings" ? "nav-link active" : "nav-link"}
                  onClick={() => navigate("admin-settings")}
                  title={t("admin.systemSettings")}
                >
                  <SlidersHorizontal aria-hidden="true" size={18} />
                  <span>{t("admin.systemSettings")}</span>
                </button>
              </div>
            ) : null}
            {user ? (
              <>
                <div className="nav-group">
                  <button
                    type="button"
                    className={activeView === "scheduled-tasks" ? "nav-link active" : "nav-link"}
                    onClick={() => navigate("scheduled-tasks")}
                    title={t("layout.scheduledTasks")}
                  >
                    <CalendarClock aria-hidden="true" size={18} />
                    <span>{t("layout.scheduledTasks")}</span>
                  </button>
                </div>
                <div className="nav-group">
                  <button
                    type="button"
                    className={activeView === "personal-settings" ? "nav-link active" : "nav-link"}
                    onClick={() => navigate("personal-settings")}
                    title={t("layout.personalSettings")}
                  >
                    <UserCog aria-hidden="true" size={18} />
                    <span>{t("layout.personalSettings")}</span>
                  </button>
                </div>
                <div className="account">
                  <span className="account-email">{user.email}</span>
                  <button
                    type="button"
                    className="icon-button"
                    onClick={() => void logout()}
                    aria-label={t("layout.signOut")}
                    title={t("layout.signOut")}
                  >
                    <LogOut aria-hidden="true" size={17} />
                  </button>
                </div>
              </>
            ) : onLogin ? (
              <div className="nav-group">
                <button
                  type="button"
                  className="nav-link"
                  onClick={() => {
                    setMobileSidebarOpen(false);
                    onLogin?.();
                  }}
                  title={t("layout.signIn")}
                >
                  <LogIn aria-hidden="true" size={18} />
                  <span>{t("layout.signIn")}</span>
                </button>
              </div>
            ) : null}
          </nav>
        </aside>
        <main className={activeView === "chat" ? "content chat-content" : "content"}>
          {children}
        </main>
        </div>
      </SidebarCollapsedContext.Provider>
    </ChatSidebarContext.Provider>
  );
}

export function useChatSidebar() {
  return useContext(ChatSidebarContext);
}

export function useSidebarCollapsed() {
  return useContext(SidebarCollapsedContext);
}
