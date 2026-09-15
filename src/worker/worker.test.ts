import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { runWorker, describeWsError } from "./helper.mjs";

const created: string[] = [];
function io(stateDir: string) {
  let stdout = ""; let stderr = "";
  return { env: { MULLION_HELPER_STATE_DIR: stateDir }, stdout: { write: (value: string) => { stdout += value; } }, stderr: { write: (value: string) => { stderr += value; } }, output: () => ({ stdout, stderr }) };
}
afterEach(() => { for (const path of created.splice(0)) rmSync(path, { recursive: true, force: true }); });

describe("private worker interface", () => {
  it("reports protocol version", async () => {
    const context = io(mkdtempSync(join(tmpdir(), "mullion-worker-"))); created.push(context.env.MULLION_HELPER_STATE_DIR);
    expect(await runWorker("version", ["--json"], context)).toBe(0);
    expect(JSON.parse(context.output().stdout)).toMatchObject({ protocol_version: 1 });
  });
  it("inspects credentials without exposing the session token", async () => {
    const state = mkdtempSync(join(tmpdir(), "mullion-worker-")); created.push(state); mkdirSync(state, { recursive: true });
    writeFileSync(join(state, "ssh-agent-bridge.json"), JSON.stringify({ baseUrl: "https://mullion.example", bridgeId: "12345678-1234-1234-1234-123456789abc", sessionId: "a".repeat(64), expiresAt: "2099-01-01T00:00:00.000Z" }));
    const context = io(state); expect(await runWorker("inspect", ["--json"], context)).toBe(0);
    expect(context.output().stdout).not.toContain("a".repeat(64)); expect(JSON.parse(context.output().stdout)).toMatchObject({ paired: true, bridge_id: "12345678-1234-1234-1234-123456789abc" });
  });
  it("rejects removed installer verbs", async () => {
    const context = io(mkdtempSync(join(tmpdir(), "mullion-worker-"))); created.push(context.env.MULLION_HELPER_STATE_DIR);
    expect(await runWorker("install", [], context)).toBe(2);
  });
});

describe("describeWsError", () => {
  it("maps a known system error code to a human phrase, keeping the code visible", () => {
    expect(describeWsError({ error: { code: "ECONNREFUSED" } })).toBe("connection refused by the server (ECONNREFUSED)");
    expect(describeWsError({ error: { code: "ENOTFOUND" } })).toBe("could not resolve the server's hostname (ENOTFOUND)");
    expect(describeWsError({ error: { code: "ETIMEDOUT" } })).toBe("connection attempt timed out (ETIMEDOUT)");
  });

  it("names the fix for a TLS trust failure", () => {
    expect(describeWsError({ error: { code: "UNABLE_TO_VERIFY_LEAF_SIGNATURE" } })).toContain("Allow self-signed TLS certificates");
    expect(describeWsError({ error: { code: "DEPTH_ZERO_SELF_SIGNED_CERT" } })).toContain("Allow self-signed TLS certificates");
  });

  it("reads the code from event.error.cause when undici wraps the connector error", () => {
    expect(describeWsError({ error: { cause: { code: "ECONNRESET" } } })).toBe("connection reset while connecting (ECONNRESET)");
  });

  it("walks multiple levels of .cause (e.g. a Happy-Eyeballs AggregateError wrapping a per-attempt error)", () => {
    expect(
      describeWsError({ error: { cause: { cause: { cause: { code: "ETIMEDOUT" } } } } }),
    ).toBe("connection attempt timed out (ETIMEDOUT)");
  });

  it("gives up past the bounded cause depth rather than looping forever on a pathological chain", () => {
    let cause: unknown = { code: "ETIMEDOUT" };
    for (let i = 0; i < 10; i++) cause = { cause };
    expect(describeWsError({ error: cause })).toBe("server did not complete the WebSocket upgrade");
  });

  it("still names an unmapped code rather than falling back to the generic message", () => {
    expect(describeWsError({ error: { code: "ESOMETHINGNEW" } })).toBe("connection error (ESOMETHINGNEW)");
  });

  it("treats a missing code as a rejected upgrade, not a network failure", () => {
    // This is what undici actually produces for a handshake that reached
    // the server and got a non-101 response back -- no system error code,
    // because nothing at the socket/DNS/TLS layer failed.
    expect(describeWsError({ error: new Error("Unexpected server response: 502") })).toBe(
      "server did not complete the WebSocket upgrade",
    );
    expect(describeWsError({})).toBe("server did not complete the WebSocket upgrade");
    expect(describeWsError(undefined)).toBe("server did not complete the WebSocket upgrade");
  });

  it("never echoes the underlying error's free-form message", () => {
    const description = describeWsError({ error: { code: "ECONNREFUSED", message: "a secret internal path leaked here" } });
    expect(description).not.toContain("secret internal path");
  });
});
