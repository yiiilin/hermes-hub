import type { User } from "./api/client";

export type AdminSettingsTab =
  | "users"
  | "models"
  | "hermes"
  | "profile"
  | "scheduler"
  | "skills"
  | "system"
  | "api-management"
  | "integration-apps"
  | "public-platform"
  | "auth";

export type PersonalSettingsTab = "personalization" | "password";

export type AppRoute =
  | { name: "home" }
  | { name: "login"; next?: string | null }
  | { name: "public-chat"; sessionId?: string | null }
  | { name: "chat"; sessionId?: string | null }
  | { name: "settings"; tab: AdminSettingsTab }
  | { name: "scheduled-tasks" }
  | { name: "personal"; tab: PersonalSettingsTab };

export const defaultAdminSettingsTab: AdminSettingsTab = "users";
export const defaultPersonalSettingsTab: PersonalSettingsTab = "personalization";

const adminSettingsTabs = new Set<AdminSettingsTab>([
  "users",
  "models",
  "hermes",
  "profile",
  "scheduler",
  "skills",
  "system",
  "api-management",
  "integration-apps",
  "public-platform",
  "auth",
]);
const personalSettingsTabs = new Set<PersonalSettingsTab>(["personalization", "password"]);

export function parseAppRoute(location: Pick<Location, "pathname" | "search">): AppRoute {
  const pathname = normalizePathname(location.pathname);
  const searchParams = new URLSearchParams(location.search);
  if (searchParams.has("invite") || searchParams.has("invite_token")) {
    return { name: "login" };
  }

  if (pathname === "/" || pathname === "") {
    return { name: "home" };
  }
  if (pathname === "/login") {
    return { name: "login", next: safeInternalNextPath(searchParams.get("next")) };
  }
  if (pathname === "/public") {
    return { name: "public-chat", sessionId: null };
  }
  if (pathname.startsWith("/public/sessions/")) {
    return {
      name: "public-chat",
      sessionId: pathSegmentAfter(pathname, "/public/sessions/"),
    };
  }
  if (pathname === "/chat") {
    return { name: "chat", sessionId: null };
  }
  if (pathname.startsWith("/chat/sessions/")) {
    return {
      name: "chat",
      sessionId: pathSegmentAfter(pathname, "/chat/sessions/"),
    };
  }
  if (pathname === "/settings") {
    return { name: "settings", tab: defaultAdminSettingsTab };
  }
  if (pathname.startsWith("/settings/")) {
    const tab = pathSegmentAfter(pathname, "/settings/");
    return {
      name: "settings",
      tab: isAdminSettingsTab(tab) ? tab : defaultAdminSettingsTab,
    };
  }
  if (pathname === "/scheduled-tasks") {
    return { name: "scheduled-tasks" };
  }
  if (pathname === "/personal") {
    return { name: "personal", tab: defaultPersonalSettingsTab };
  }
  if (pathname.startsWith("/personal/")) {
    const tab = pathSegmentAfter(pathname, "/personal/");
    return {
      name: "personal",
      tab: isPersonalSettingsTab(tab) ? tab : defaultPersonalSettingsTab,
    };
  }

  return { name: "home" };
}

export function parseAppPath(path: string): AppRoute {
  const url = new URL(path, window.location.origin);
  return parseAppRoute(url);
}

export function buildAppPath(route: AppRoute): string {
  switch (route.name) {
    case "home":
      return "/";
    case "login": {
      const next = safeInternalNextPath(route.next ?? null);
      return next ? `/login?next=${encodeURIComponent(next)}` : "/login";
    }
    case "public-chat":
      return route.sessionId
        ? `/public/sessions/${encodeURIComponent(route.sessionId)}`
        : "/public";
    case "chat":
      return route.sessionId ? `/chat/sessions/${encodeURIComponent(route.sessionId)}` : "/chat";
    case "settings":
      return `/settings/${route.tab}`;
    case "scheduled-tasks":
      return "/scheduled-tasks";
    case "personal":
      return `/personal/${route.tab}`;
  }
}

export function pushAppRoute(route: AppRoute) {
  setAppRoute(route, "push");
}

export function replaceAppRoute(route: AppRoute) {
  setAppRoute(route, "replace");
}

export function normalizeAppRouteForSessionState({
  publicPlatformEnabled,
  route,
  user,
}: {
  publicPlatformEnabled: boolean;
  route: AppRoute;
  user: User | null;
}): AppRoute {
  if (user) {
    if (route.name === "settings" && user.role !== "admin") {
      return { name: "chat", sessionId: null };
    }
    if (route.name === "home" || route.name === "login" || route.name === "public-chat") {
      const nextRoute =
        route.name === "login" && route.next ? parseAppPath(route.next) : { name: "chat" as const };
      return normalizeAppRouteForSessionState({
        publicPlatformEnabled,
        route: nextRoute,
        user,
      });
    }
    return route;
  }

  if (route.name === "login") {
    return route;
  }
  if (route.name === "public-chat") {
    return publicPlatformEnabled ? route : { name: "login", next: null };
  }
  if (route.name === "home") {
    return publicPlatformEnabled
      ? { name: "public-chat", sessionId: null }
      : { name: "login", next: null };
  }

  return { name: "login", next: buildAppPath(route) };
}

export function appRoutesEqual(left: AppRoute, right: AppRoute) {
  return buildAppPath(left) === buildAppPath(right);
}

export function safeInternalNextPath(value: string | null | undefined) {
  if (!value || !value.startsWith("/") || value.startsWith("//")) {
    return null;
  }
  try {
    const parsed = new URL(value, window.location.origin);
    if (parsed.origin !== window.location.origin) {
      return null;
    }
    return `${parsed.pathname}${parsed.search}${parsed.hash}`;
  } catch {
    return null;
  }
}

function setAppRoute(route: AppRoute, mode: "push" | "replace") {
  const nextPath = buildAppPath(route);
  const currentPath = `${window.location.pathname}${window.location.search}${window.location.hash}`;
  if (currentPath === nextPath) {
    return;
  }
  if (mode === "push") {
    window.history.pushState({}, "", nextPath);
  } else {
    window.history.replaceState({}, "", nextPath);
  }
}

function normalizePathname(pathname: string) {
  return pathname.endsWith("/") && pathname.length > 1 ? pathname.slice(0, -1) : pathname;
}

function pathSegmentAfter(pathname: string, prefix: string) {
  const rawValue = pathname.slice(prefix.length).split("/")[0] ?? "";
  try {
    return decodeURIComponent(rawValue);
  } catch {
    return rawValue;
  }
}

function isAdminSettingsTab(value: string): value is AdminSettingsTab {
  return adminSettingsTabs.has(value as AdminSettingsTab);
}

function isPersonalSettingsTab(value: string): value is PersonalSettingsTab {
  return personalSettingsTabs.has(value as PersonalSettingsTab);
}
