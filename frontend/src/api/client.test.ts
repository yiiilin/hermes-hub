import { describe, expect, it, vi } from "vitest";

import { ApiRequestError, createApiClient, createMockApiClient } from "./client";

describe("api client errors", () => {
  it("uses plain-text 422 response bodies as the error message", async () => {
    const fetchMock = vi
      .spyOn(globalThis, "fetch")
      .mockResolvedValue(
        new Response("authorization_code_ttl_seconds: invalid type: null, expected u64", {
          status: 422,
          statusText: "Unprocessable Entity",
          headers: {
            "Content-Type": "text/plain",
          },
        }),
      );

    const client = createApiClient();

    await expect(
      client.updateIntegrationApp("integration-app-1", {
        name: "CRM",
        enabled: true,
        redirect_uri: "https://crm.example/callback",
        scopes: "openid profile email",
        authorization_code_ttl_seconds: 600,
        hidden_session_idle_timeout_seconds: 3600,
        default_tool_timeout_seconds: 60,
        max_tool_timeout_seconds: 300,
      }),
    ).rejects.toSatisfy((error: unknown) => {
      expect(error).toBeInstanceOf(ApiRequestError);
      expect((error as ApiRequestError).message).toBe(
        "authorization_code_ttl_seconds: invalid type: null, expected u64",
      );
      return true;
    });

    fetchMock.mockRestore();
  });

  it("mock integration tools lookup matches backend not-found behavior", async () => {
    const client = createMockApiClient();

    await expect(client.listIntegrationAppTools("missing-app")).rejects.toThrow(
      "integration app not found",
    );
  });

  it("mock integration app creation matches backend integration id de-duplication", async () => {
    const client = createMockApiClient({
      initialIntegrationApps: [
        {
          id: "integration-app-1",
          integration_id: "crm",
          name: "CRM",
          enabled: true,
          client_id: "client-1",
          redirect_uri: "https://crm.example/callback",
          scopes: "openid profile email",
          authorization_code_ttl_seconds: 600,
          hidden_session_idle_timeout_seconds: 3600,
          default_tool_timeout_seconds: 60,
          max_tool_timeout_seconds: 300,
          last_used_at: null,
          created_at: 1,
          updated_at: 1,
        },
      ],
    });

    const created = await client.createIntegrationApp({
      name: "CRM",
      enabled: true,
      redirect_uri: "https://crm-2.example/callback",
      scopes: "openid profile email",
      authorization_code_ttl_seconds: 600,
      hidden_session_idle_timeout_seconds: 3600,
      default_tool_timeout_seconds: 60,
      max_tool_timeout_seconds: 300,
    });

    expect(created.app.integration_id).toBe("crm-2");
  });
});
