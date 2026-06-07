import { describe, expect, it } from "vitest";
import {
  buildAppPath,
  normalizeAppRouteForSessionState,
  parseAppRoute,
  safeInternalNextPath,
  type AppRoute,
} from "./navigation";
import type { User } from "./api/client";

function routeFrom(path: string): AppRoute {
  const url = new URL(path, window.location.origin);
  return parseAppRoute(url);
}

const adminUser: User = {
  id: "user-1",
  email: "admin@example.com",
  role: "admin",
  status: "active",
};

const regularUser: User = {
  ...adminUser,
  role: "user",
};

describe("navigation", () => {
  it("parses and builds stable app paths", () => {
    expect(routeFrom("/chat/sessions/session-1")).toEqual({
      name: "chat",
      sessionId: "session-1",
    });
    expect(routeFrom("/settings/auth")).toEqual({ name: "settings", tab: "auth" });
    expect(routeFrom("/settings/api-management")).toEqual({
      name: "settings",
      tab: "api-management",
    });
    expect(routeFrom("/settings/integration-apps")).toEqual({
      name: "settings",
      tab: "integration-apps",
    });
    expect(routeFrom("/personal/password")).toEqual({ name: "personal", tab: "password" });
    expect(routeFrom("/public/sessions/public-1")).toEqual({
      name: "public-chat",
      sessionId: "public-1",
    });

    expect(buildAppPath({ name: "settings", tab: "public-platform" })).toBe(
      "/settings/public-platform",
    );
    expect(buildAppPath({ name: "settings", tab: "api-management" })).toBe(
      "/settings/api-management",
    );
    expect(buildAppPath({ name: "settings", tab: "integration-apps" })).toBe(
      "/settings/integration-apps",
    );
    expect(buildAppPath({ name: "personal", tab: "password" })).toBe("/personal/password");
    expect(buildAppPath({ name: "chat", sessionId: "session/with/slash" })).toBe(
      "/chat/sessions/session%2Fwith%2Fslash",
    );
  });

  it("normalizes routes for anonymous, admin, and regular users", () => {
    expect(
      normalizeAppRouteForSessionState({
        publicPlatformEnabled: true,
        route: { name: "home" },
        user: null,
      }),
    ).toEqual({ name: "public-chat", sessionId: null });
    expect(
      normalizeAppRouteForSessionState({
        publicPlatformEnabled: false,
        route: { name: "chat", sessionId: "session-1" },
        user: null,
      }),
    ).toEqual({ name: "login", next: "/chat/sessions/session-1" });
    expect(
      normalizeAppRouteForSessionState({
        publicPlatformEnabled: true,
        route: { name: "settings", tab: "auth" },
        user: regularUser,
      }),
    ).toEqual({ name: "chat", sessionId: null });
    expect(
      normalizeAppRouteForSessionState({
        publicPlatformEnabled: true,
        route: { name: "settings", tab: "auth" },
        user: adminUser,
      }),
    ).toEqual({ name: "settings", tab: "auth" });
  });

  it("only accepts same-origin relative next paths", () => {
    expect(safeInternalNextPath("/settings/auth?x=1#section")).toBe(
      "/settings/auth?x=1#section",
    );
    expect(safeInternalNextPath("https://evil.example/settings/auth")).toBeNull();
    expect(safeInternalNextPath("//evil.example/settings/auth")).toBeNull();
    expect(safeInternalNextPath("settings/auth")).toBeNull();
  });
});
