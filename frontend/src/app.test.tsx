import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { App } from "./app";

describe("App", () => {
  it("renders the Hermes Hub shell", () => {
    render(<App />);

    expect(screen.getByRole("heading", { name: "Hermes Hub" })).toBeInTheDocument();
  });
});
