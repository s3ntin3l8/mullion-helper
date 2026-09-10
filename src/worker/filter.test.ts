import { describe, expect, it } from "vitest";
import { MAX_FRAME_BYTES, SSH_AGENT_FAILURE_FRAME, SSH_AGENTC_ADD_IDENTITY, SSH_AGENTC_REQUEST_IDENTITIES, SSH_AGENTC_SIGN_REQUEST, SignOnlyFilter, SshAgentFrameTooLargeError } from "./filter.mjs";
import protocol from "./ssh-agent-protocol-v1.json";
import { SSH_AGENT_REQUEST_TYPE_VECTORS } from "./filter.mjs";

function frame(type: number, body = Buffer.alloc(0)) {
  const result = Buffer.alloc(5 + body.length); result.writeUInt32BE(1 + body.length); result[4] = type; body.copy(result, 5); return result;
}

describe("sign-only SSH-agent filter", () => {
  it("matches the server-owned protocol fixture", () => {
    expect(SSH_AGENT_REQUEST_TYPE_VECTORS).toEqual(protocol.vectors);
    expect(MAX_FRAME_BYTES).toBe(protocol.wireFormat.maxFrameBytes);
    expect(SSH_AGENT_FAILURE_FRAME.toString("hex")).toBe(protocol.sshAgentFailure.frameHex);
  });
  it.each([SSH_AGENTC_REQUEST_IDENTITIES, SSH_AGENTC_SIGN_REQUEST])("forwards allowed request %s byte-for-byte", (type) => {
    const input = frame(type, Buffer.from("abcd")); expect(new SignOnlyFilter().feed(input).forward).toEqual([input]);
  });
  it("blocks mutating and unknown requests with SSH_AGENT_FAILURE", () => {
    for (const type of [SSH_AGENTC_ADD_IDENTITY, 200]) {
      const result = new SignOnlyFilter().feed(frame(type)); expect(result.forward).toEqual([]); expect(result.reject).toEqual([SSH_AGENT_FAILURE_FRAME]);
    }
  });
  it("reassembles split frames", () => {
    const input = frame(SSH_AGENTC_SIGN_REQUEST, Buffer.from("payload")); const filter = new SignOnlyFilter();
    expect(filter.feed(input.subarray(0, 3)).forward).toEqual([]); expect(filter.feed(input.subarray(3)).forward).toEqual([input]);
  });
  it("tears down on an oversized declared body", () => {
    const input = Buffer.alloc(4); input.writeUInt32BE(MAX_FRAME_BYTES + 1); expect(() => new SignOnlyFilter().feed(input)).toThrow(SshAgentFrameTooLargeError);
  });
});
