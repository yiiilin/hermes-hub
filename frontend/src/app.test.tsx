import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "./app";
import { createMockApiClient } from "./api/client";

describe("App", () => {
  it("renders the authenticated admin workspace and can send a Hermes prompt", async () => {
    render(<App apiClient={createMockApiClient()} />);

    expect(await screen.findByRole("heading", { name: "Users" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Invites" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Channels" })).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Session" })).toBeInTheDocument();

    fireEvent.change(screen.getByLabelText("Prompt"), {
      target: { value: "hello" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send to Hermes" }));

    expect(await screen.findByText(/data: hello/)).toBeInTheDocument();
  });

  it("renders login and authenticates with email and password", async () => {
    const client = createMockApiClient();
    await client.logout();

    render(<App apiClient={client} />);

    expect(await screen.findByRole("heading", { name: "Hermes Hub" })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText("Email"), {
      target: { value: "admin@example.com" },
    });
    fireEvent.change(screen.getByLabelText("Password"), {
      target: { value: "admin-password-123" },
    });
    fireEvent.click(screen.getAllByRole("button", { name: "Sign in" }).at(-1)!);

    await waitFor(() => {
      expect(screen.getByRole("heading", { name: "Users" })).toBeInTheDocument();
    });
  });
});
