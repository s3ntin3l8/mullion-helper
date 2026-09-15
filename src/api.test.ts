import { afterEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.fn();
const getVersionMock = vi.fn();
const getNameMock = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invokeMock(...args) }));
vi.mock("@tauri-apps/api/app", () => ({
  getVersion: (...args: unknown[]) => getVersionMock(...args),
  getName: (...args: unknown[]) => getNameMock(...args),
}));

afterEach(() => {
  invokeMock.mockReset();
  getVersionMock.mockReset();
  getNameMock.mockReset();
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

describe("api.version", () => {
  it("resolves to null outside a Tauri window, without calling getVersion", async () => {
    const { api } = await import("./api");
    await expect(api.version()).resolves.toBeNull();
    expect(getVersionMock).not.toHaveBeenCalled();
  });

  it("calls getVersion when running inside Tauri", async () => {
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    getVersionMock.mockResolvedValue("0.1.10");
    const { api } = await import("./api");
    await expect(api.version()).resolves.toBe("0.1.10");
    expect(getVersionMock).toHaveBeenCalledTimes(1);
  });
});

describe("api.appName", () => {
  it("resolves to the fallback name outside a Tauri window, without calling getName", async () => {
    const { api } = await import("./api");
    await expect(api.appName()).resolves.toBe("Mullion Helper");
    expect(getNameMock).not.toHaveBeenCalled();
  });

  it("calls getName when running inside Tauri", async () => {
    (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ = {};
    getNameMock.mockResolvedValue("Mullion Helper");
    const { api } = await import("./api");
    await expect(api.appName()).resolves.toBe("Mullion Helper");
    expect(getNameMock).toHaveBeenCalledTimes(1);
  });
});
