import { describe, expect, it } from "vitest";
import { decodePairingPayload } from "./pairing.mjs";

const encode = (value: unknown) => Buffer.from(JSON.stringify(value), "utf8").toString("base64url");

describe("pairing payload decoder", () => {
  it("decodes the server protocol shape", () => {
    expect(decodePairingPayload(encode({ baseUrl: "https://mullion.example", code: "abc123" }))).toEqual({ baseUrl: "https://mullion.example", code: "abc123" });
  });
  it.each(["garbage", "", "A".repeat(16_000)])("fails closed for invalid payloads", (value) => expect(decodePairingPayload(value)).toBeNull());
  it.each(["file:///tmp/x", "ssh://example.test", "javascript:alert(1)"])("rejects non-HTTP URLs", (baseUrl) => expect(decodePairingPayload(encode({ baseUrl, code: "abc" }))).toBeNull());
});
