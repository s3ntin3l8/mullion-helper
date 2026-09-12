import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";

const WORKER_CRASH = "Fatal process out of memory: Failed to reserve virtual memory for CodeRange\n----- Native stack trace -----\n1: node::Start(int, char**)\n2: start";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn((command: string) =>
    command === "pair_bridge"
      ? Promise.reject(WORKER_CRASH)
      : Promise.reject(new Error(`unexpected invoke in test: ${command}`)),
  ),
}));

describe("Mullion Helper window", () => {
  it("shows the pairing workflow when no bridge is paired", async () => {
    const { container } = render(<App />);
    expect(await screen.findByText("Connect this computer")).toBeVisible();
    expect(screen.getByRole("button", { name: "Pair and start" })).toBeDisabled();
    expect(screen.getByText("Closing this window keeps the tray app running.")).toBeVisible();
    expect(container.querySelector("header svg.brand-mark")).toBeInTheDocument();
    expect(container.querySelector("header img")).not.toBeInTheDocument();
  });

  it("shows a failed worker pairing as a summary line with the rest collapsed behind details", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("Connect this computer");

    await user.type(screen.getByPlaceholderText("Paste pairing payload"), "some-payload");
    await user.click(screen.getByRole("button", { name: "Pair and start" }));

    const alert = await screen.findByRole("alert");
    const summary = alert.querySelector("p");
    expect(summary).toHaveTextContent("Fatal process out of memory: Failed to reserve virtual memory for CodeRange");
    expect(summary).not.toHaveTextContent("Native stack trace");

    const details = alert.querySelector("details");
    expect(details).not.toBeNull();
    expect(details).not.toHaveAttribute("open");
    expect(details).toHaveTextContent("----- Native stack trace -----");
    expect(details).toHaveTextContent("node::Start(int, char**)");

    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    await user.click(screen.getByRole("button", { name: "Copy details" }));
    expect(writeText).toHaveBeenCalledWith("----- Native stack trace -----\n1: node::Start(int, char**)\n2: start");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });
});
