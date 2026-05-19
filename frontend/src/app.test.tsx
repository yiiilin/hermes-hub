import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "./app";
import { createMockApiClient } from "./api/client";

describe("App", () => {
  it("renders login, admin, channel, and session surfaces", async () => {
    render(<App apiClient={createMockApiClient()} />);

    expect(screen.getByRole("heading", { name: "Hermes Hub" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeInTheDocument();
    expect(await screen.findByText("Users")).toBeInTheDocument();
    expect(await screen.findByText("Model configuration")).toBeInTheDocument();
    expect(await screen.findByText("Channels")).toBeInTheDocument();
    expect(await screen.findByRole("heading", { name: "Session" })).toBeInTheDocument();
  });
});
