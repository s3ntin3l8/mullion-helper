import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { App } from "./App";

describe("Mullion Helper window", () => {
  it("shows the pairing workflow when no bridge is paired", async () => {
    const { container } = render(<App />);
    expect(await screen.findByText("Connect this computer")).toBeVisible();
    expect(screen.getByRole("button", { name: "Pair and start" })).toBeDisabled();
    expect(screen.getByText("Closing this window keeps the tray app running.")).toBeVisible();
    expect(container.querySelector("header svg.brand-mark")).toBeInTheDocument();
    expect(container.querySelector("header img")).not.toBeInTheDocument();
  });
});
