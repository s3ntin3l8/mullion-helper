import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "./App";
import type { AgentSocketList, BridgeStatus, Settings } from "./types";

const WORKER_CRASH = "Fatal process out of memory: Failed to reserve virtual memory for CodeRange\n----- Native stack trace -----\n1: node::Start(int, char**)\n2: start";

// jsdom never has __TAURI_INTERNALS__, so the real api.ts always takes its
// "not in Tauri" branch and never calls invoke — mocking @tauri-apps/api/core
// (as this file used to) can never reach anything past the unpaired demo
// status. Mocking the app's own api module instead makes every state,
// including paired/connected, reachable and stubbable per-test.
const { mockApi } = vi.hoisted(() => {
  const unpairedStatus: BridgeStatus = { state: "unpaired", base_url: null, bridge_id: null, detail: null, retry_in_ms: null, updated_at: new Date().toISOString(), agent_identities: null, consecutive_connect_failures: 0 };
  const defaultSettings: Settings = { ssh_auth_sock: "", insecure: false, launch_at_login: false };
  return {
    mockApi: {
      isDesktop: false,
      status: vi.fn(async (): Promise<BridgeStatus> => unpairedStatus),
      settings: vi.fn(async (): Promise<Settings> => defaultSettings),
      listAgentSockets: vi.fn(async (): Promise<AgentSocketList> => ({ candidates: [], chosen: null })),
      pair: vi.fn(async (): Promise<BridgeStatus> => { throw WORKER_CRASH; }),
      unpair: vi.fn(async (): Promise<BridgeStatus> => unpairedStatus),
      start: vi.fn(async (): Promise<BridgeStatus> => unpairedStatus),
      pause: vi.fn(async (): Promise<BridgeStatus> => unpairedStatus),
      saveSettings: vi.fn(async (settings: Settings): Promise<Settings> => settings),
      checkForUpdates: vi.fn(async () => ({ available: false, version: null as string | null })),
      installUpdate: vi.fn(async (): Promise<void> => undefined),
      diagnosticsPath: vi.fn(async (): Promise<string | null> => null),
      version: vi.fn(async (): Promise<string | null> => "0.1.10"),
      appName: vi.fn(async (): Promise<string> => "Mullion Helper"),
    },
  };
});

vi.mock("./api", () => ({ api: mockApi }));

const connectedStatus: BridgeStatus = { state: "connected", base_url: "https://mullion.example", bridge_id: "bridge-123", detail: null, retry_in_ms: null, updated_at: new Date().toISOString(), agent_identities: null, consecutive_connect_failures: 0 };
const unpairedStatus: BridgeStatus = { state: "unpaired", base_url: null, bridge_id: null, detail: null, retry_in_ms: null, updated_at: new Date().toISOString(), agent_identities: null, consecutive_connect_failures: 0 };

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
    expect(writeText).toHaveBeenCalledWith("Mullion Helper 0.1.10\n----- Native stack trace -----\n1: node::Start(int, char**)\n2: start");
    expect(await screen.findByRole("button", { name: "Copied" })).toBeInTheDocument();
  });

  it("shows the running version in the footer", async () => {
    render(<App />);
    await screen.findByText("Connect this computer");
    expect(await screen.findByText("· v0.1.10")).toBeVisible();
  });

  it("offers re-pair and unpair once a bridge is already paired, and unpair round-trips through the API", async () => {
    const user = userEvent.setup();
    mockApi.status.mockResolvedValueOnce(connectedStatus);
    mockApi.unpair.mockResolvedValueOnce(unpairedStatus);
    render(<App />);

    await screen.findByText("Bridge connected");
    // The first-time-setup panel must not also render once paired.
    expect(screen.queryByText("Connect this computer")).not.toBeInTheDocument();

    expect(screen.getByText("Re-pair this computer")).toBeVisible();
    expect(screen.queryByText("Remove this computer's pairing?")).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Unpair this computer" }));
    expect(screen.getByText("Remove this computer's pairing?")).toBeVisible();

    await user.click(screen.getByRole("button", { name: "Unpair" }));

    expect(mockApi.unpair).toHaveBeenCalledTimes(1);
    await screen.findByText("Connect this computer");
    expect(screen.queryByText("Remove this computer's pairing?")).not.toBeInTheDocument();
  });

  it("collapses the unpair confirm row and refreshes the stale status when the unpair call itself fails", async () => {
    const user = userEvent.setup();
    // By the time unpair() can reject, the backend has already stopped
    // everything -- simulate it reporting that via the post-rejection
    // refresh, rather than leaving the pre-unpair "connected" status
    // showing as if nothing had changed.
    const errorStatus: BridgeStatus = { state: "error", base_url: null, bridge_id: null, detail: "could not record completion of the legacy migration", retry_in_ms: null, updated_at: new Date().toISOString(), agent_identities: null, consecutive_connect_failures: 0 };
    mockApi.status.mockResolvedValueOnce(connectedStatus).mockResolvedValueOnce(errorStatus);
    mockApi.unpair.mockRejectedValueOnce(new Error("worker unreachable"));
    render(<App />);

    await screen.findByText("Bridge connected");
    await user.click(screen.getByRole("button", { name: "Unpair this computer" }));
    await user.click(screen.getByRole("button", { name: "Unpair" }));

    await screen.findByRole("alert");
    expect(screen.queryByText("Remove this computer's pairing?")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Unpair this computer" })).toBeVisible();
    // The refresh triggered by the rejection must have actually landed --
    // the card should no longer be showing the stale pre-unpair status.
    expect(await screen.findByText("Bridge needs attention")).toBeVisible();
    expect(screen.queryByText("Bridge connected")).not.toBeInTheDocument();
  });

  it("lists detected SSH agent candidates in the dropdown, labelled with their identity counts", async () => {
    mockApi.listAgentSockets.mockResolvedValueOnce({
      candidates: [
        { path: "/1p", label: "1Password", reachable: true, identities: 3 },
        { path: "/launchd", label: "macOS login agent", reachable: true, identities: 0 },
      ],
      chosen: "/1p",
    });
    render(<App />);
    await screen.findByText("Connect this computer");

    expect(await screen.findByRole("option", { name: "Auto-detect (1Password — 3 identities)" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "1Password — 3 identities" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "macOS login agent — 0 identities" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Custom path…" })).toBeInTheDocument();
  });

  it("keeps a stored socket path that isn't in the detected list selected as Custom, with the text input visible", async () => {
    mockApi.settings.mockResolvedValueOnce({ ssh_auth_sock: "/opt/custom-agent.sock", insecure: false, launch_at_login: false });
    mockApi.listAgentSockets.mockResolvedValueOnce({ candidates: [{ path: "/1p", label: "1Password", reachable: true, identities: 3 }], chosen: "/1p" });
    render(<App />);
    await screen.findByText("Connect this computer");

    const select = await screen.findByRole("combobox") as HTMLSelectElement;
    await waitFor(() => expect(select.value).toBe("__custom__"));
    expect(screen.getByLabelText("Custom socket path")).toHaveValue("/opt/custom-agent.sock");
  });

  it("selecting Custom path… reveals the free-text socket input", async () => {
    const user = userEvent.setup();
    mockApi.listAgentSockets.mockResolvedValueOnce({ candidates: [{ path: "/1p", label: "1Password", reachable: true, identities: 3 }], chosen: "/1p" });
    render(<App />);
    await screen.findByText("Connect this computer");

    expect(screen.queryByLabelText("Custom socket path")).not.toBeInTheDocument();
    const select = await screen.findByRole("combobox");
    await user.selectOptions(select, "Custom path…");
    expect(await screen.findByLabelText("Custom socket path")).toBeInTheDocument();
  });

  it("re-detects the agent candidate list when Re-detect is clicked", async () => {
    const user = userEvent.setup();
    render(<App />);
    await screen.findByText("Connect this computer");
    const callsBeforeClick = mockApi.listAgentSockets.mock.calls.length;

    await user.click(screen.getByRole("button", { name: "Re-detect" }));

    await waitFor(() => expect(mockApi.listAgentSockets.mock.calls.length).toBeGreaterThan(callsBeforeClick));
  });

  it("warns when the agent is connected but reports zero identities", async () => {
    mockApi.status.mockResolvedValueOnce({ ...connectedStatus, agent_identities: 0 });
    render(<App />);
    await screen.findByText("Bridge connected");
    expect(await screen.findByText(/no identities loaded/)).toBeVisible();
  });

  it("does not show the zero-identity warning outside a connected state, even if a stale count says zero", async () => {
    // Regression for the Hermes-flagged stale-stamp bug: a prior
    // resolution's identity count could otherwise leak onto an unrelated
    // state (e.g. AgentUnavailable) and render a contradictory warning.
    mockApi.status.mockResolvedValueOnce({ ...unpairedStatus, state: "agent_unavailable", agent_identities: 0 });
    render(<App />);
    await screen.findByText("SSH agent unavailable");
    expect(screen.queryByText(/no identities loaded/)).not.toBeInTheDocument();
  });

  it("renders the retry countdown while reconnecting", async () => {
    mockApi.status.mockResolvedValueOnce({ ...connectedStatus, state: "reconnecting", retry_in_ms: 5000, consecutive_connect_failures: 1 });
    render(<App />);
    expect(await screen.findByText("Retrying in 5s")).toBeVisible();
  });

  it("escalates the reconnecting indicator once the failure streak passes the threshold", async () => {
    mockApi.status.mockResolvedValueOnce({ ...connectedStatus, state: "reconnecting", retry_in_ms: 30000, consecutive_connect_failures: 5 });
    const { container } = render(<App />);
    expect(await screen.findByText(/this has been failing for a while/)).toBeVisible();
    expect(container.querySelector(".orb.reconnecting.escalated")).toBeInTheDocument();
  });

  it("does not escalate a fresh reconnect attempt", async () => {
    mockApi.status.mockResolvedValueOnce({ ...connectedStatus, state: "reconnecting", retry_in_ms: 1000, consecutive_connect_failures: 1 });
    const { container } = render(<App />);
    await screen.findByText("Retrying in 1s");
    expect(screen.queryByText(/this has been failing for a while/)).not.toBeInTheDocument();
    expect(container.querySelector(".orb.reconnecting.escalated")).not.toBeInTheDocument();
  });
});
