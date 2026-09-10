import { mkdtempSync, rmSync, writeFileSync, mkdirSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import { runWorker } from "./helper.mjs";

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
