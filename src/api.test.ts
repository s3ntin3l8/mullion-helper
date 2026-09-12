import { afterEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));

afterEach(() => {
  invokeMock.mockReset();
  delete (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  vi.resetModules();
});

describe("api.diagnosticsPath", () => {
  it("resolves to null outside a Tauri window, without invoking the backend", async () => {
    const { api } = await import("./api");
    await expect(api.diagnosticsPath()).resolves.toBeNull();
    expect(invokeMock).not.toHaveBeenCalled();
  });

  it("invokes diagnostics_path when running inside Tauri", async () => {
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    invokeMock.mockResolvedValue("/home/user/.local/share/mullion-helper/logs");
    const { api } = await import("./api");
    await expect(api.diagnosticsPath()).resolves.toBe("/home/user/.local/share/mullion-helper/logs");
    expect(invokeMock).toHaveBeenCalledWith("diagnostics_path");
  });
});
