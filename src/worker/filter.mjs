// Issue #820 (round 4, tray-repo prerequisites) — a minimal, dependency-free
// port of src/services/ssh-agent-filter.ts's sign-only classifier for the
// private bundled worker. Deliberately NOT imported from src/services/: same
// reasoning as ssh-agent-bridge-mux.mjs's own header comment — this file
// runs standalone on a laptop with no Mullion checkout at all, bundled
// directly into the Node SEA (scripts/build-worker-sea.mjs), and the
// `.mjs` CLI tree is zero-dependency, `node:` builtins only, no build step.
// Both sides of this classifier are deliberately maintained as two separate
// implementations; a change to SSH_AGENT_REQUEST_TYPE_VECTORS in
// ssh-agent-filter.ts must be mirrored here by hand — grep this repo for
// "ssh-agent-filter.ts" if you're touching one and forget the other exists.
//
// Unlike ssh-agent-bridge-mux.mjs (a hand-mirrored PORT with no shared
// source of truth beyond code review), this twin's own test suite
// (test/cli/ssh-agent-filter.test.ts) additionally validates against
// test/fixtures/ssh-agent-filter-vectors.json — the machine-readable
// fixture round 4 PR1 generates FROM ssh-agent-filter.ts's own table. Both
// implementations are provably checked against that one shared source, not
// just against each other by hand, closing (for this module) the exact
// drift risk the mux twin's own comment warns about.
//
// This module is now the AUTHORITATIVE enforcement point the TypeScript
// module's own header comment has described since issue #820's original
// design: a compromised or malicious primary (or a bug anywhere in the
// mux/routes wiring between it and the laptop) must not be able to mutate
// the laptop's SSH agent — only ask it to list identities or produce a
// signature. src/services/ssh-agent-filter.ts's own instance
// (ssh-agent-relay.ts) is defense in depth on the primary-side leg, not the
// primary enforcement boundary — see that module's own comment.
//
// This module only ever classifies REQUESTS (the client->agent direction,
// SSH_AGENTC_* below) — the real agent's own REPLIES (SSH_AGENT_* success/
// failure/identity-answer/sign-response) are always relayed unfiltered in
// the other direction. They originate from something already trusted (the
// real local agent, only ever replying to a request this filter already
// let through), so there is nothing for a reply-direction filter to guard
// against.
//
// Wire format (draft-miller-ssh-agent, the protocol OpenSSH/1Password/every
// real ssh-agent speaks): each message is a 4-byte big-endian length prefix
// followed by that many bytes of body, whose first byte is the message
// type. Framing here deliberately mirrors ssh-agent-bridge-mux.mjs's own
// decodeFrame — fixed-width header, fail-closed on anything malformed —
// but is a distinct length-prefixed format (SSH agent protocol's own), not
// this repo's mux frame format; the two must not be conflated.

export const LENGTH_PREFIX_BYTES = 4;

// Matches CHANNEL_WINDOW_BYTES in ssh-agent-bridge-mux.mjs — see
// ssh-agent-filter.ts's own doc comment for why this ceiling was chosen.
// Issue #1059 — this value is also pinned in
// test/fixtures/ssh-agent-filter-vectors.json (`wireFormat.maxFrameBytes`),
// and test/cli/ssh-agent-filter-constants.test.ts asserts this export
// matches the fixture AND the TS twin's matching export — a hand-edit
// that changes only one side fails the test instead of silently drifting.
export const MAX_FRAME_BYTES = 256 * 1024;

// SSH_AGENTC_* — client REQUEST message types this filter classifies.
export const SSH_AGENTC_REQUEST_RSA_IDENTITIES = 1; // legacy v1 protocol
export const SSH_AGENTC_ADD_RSA_IDENTITY = 7; // legacy v1, mutating
export const SSH_AGENTC_REMOVE_RSA_IDENTITY = 8; // legacy v1, mutating
export const SSH_AGENTC_REMOVE_ALL_RSA_IDENTITIES = 9; // legacy v1, mutating
export const SSH_AGENTC_REQUEST_IDENTITIES = 11;
export const SSH_AGENTC_SIGN_REQUEST = 13;
export const SSH_AGENTC_ADD_IDENTITY = 17;
export const SSH_AGENTC_REMOVE_IDENTITY = 18;
export const SSH_AGENTC_REMOVE_ALL_IDENTITIES = 19;
export const SSH_AGENTC_ADD_SMARTCARD_KEY = 20;
export const SSH_AGENTC_REMOVE_SMARTCARD_KEY = 21;
export const SSH_AGENTC_LOCK = 22;
export const SSH_AGENTC_UNLOCK = 23;
export const SSH_AGENTC_ADD_RSA_ID_CONSTRAINED = 24; // legacy v1
export const SSH_AGENTC_ADD_ID_CONSTRAINED = 25;
export const SSH_AGENTC_ADD_SMARTCARD_KEY_CONSTRAINED = 26;
export const SSH_AGENTC_EXTENSION = 27;

/** The single reply this filter ever synthesizes. Sent in place of relaying
 * a blocked request to the real agent — never a reply the real agent
 * itself produced. */
export const SSH_AGENT_FAILURE = 5;

/**
 * The conformance table: every known `SSH_AGENTC_*` request type mapped to
 * its allow/block decision and a human name. Must match
 * src/services/ssh-agent-filter.ts's own SSH_AGENT_REQUEST_TYPE_VECTORS
 * exactly, and test/cli/ssh-agent-filter.test.ts asserts this table matches
 * test/fixtures/ssh-agent-filter-vectors.json byte for byte, not just "by
 * eye" against the TS source.
 *
 * Only `SSH_AGENTC_REQUEST_IDENTITIES` and `SSH_AGENTC_SIGN_REQUEST` are
 * allowed — everything else (in particular every ADD_IDENTITY, REMOVE_*,
 * LOCK, UNLOCK, and EXTENSION message) is blocked, INCLUDING types not
 * listed here at all: `classify()` below defaults to blocked for any type
 * absent from this table, so a future/vendor/unrecognized message type is
 * refused by default rather than accidentally let through.
 */
export const SSH_AGENT_REQUEST_TYPE_VECTORS = [
  {
    type: SSH_AGENTC_REQUEST_RSA_IDENTITIES,
    name: "SSH_AGENTC_REQUEST_RSA_IDENTITIES",
    allowed: false,
  },
  { type: SSH_AGENTC_ADD_RSA_IDENTITY, name: "SSH_AGENTC_ADD_RSA_IDENTITY", allowed: false },
  { type: SSH_AGENTC_REMOVE_RSA_IDENTITY, name: "SSH_AGENTC_REMOVE_RSA_IDENTITY", allowed: false },
  {
    type: SSH_AGENTC_REMOVE_ALL_RSA_IDENTITIES,
    name: "SSH_AGENTC_REMOVE_ALL_RSA_IDENTITIES",
    allowed: false,
  },
  { type: SSH_AGENTC_REQUEST_IDENTITIES, name: "SSH_AGENTC_REQUEST_IDENTITIES", allowed: true },
  { type: SSH_AGENTC_SIGN_REQUEST, name: "SSH_AGENTC_SIGN_REQUEST", allowed: true },
  { type: SSH_AGENTC_ADD_IDENTITY, name: "SSH_AGENTC_ADD_IDENTITY", allowed: false },
  { type: SSH_AGENTC_REMOVE_IDENTITY, name: "SSH_AGENTC_REMOVE_IDENTITY", allowed: false },
  {
    type: SSH_AGENTC_REMOVE_ALL_IDENTITIES,
    name: "SSH_AGENTC_REMOVE_ALL_IDENTITIES",
    allowed: false,
  },
  { type: SSH_AGENTC_ADD_SMARTCARD_KEY, name: "SSH_AGENTC_ADD_SMARTCARD_KEY", allowed: false },
  {
    type: SSH_AGENTC_REMOVE_SMARTCARD_KEY,
    name: "SSH_AGENTC_REMOVE_SMARTCARD_KEY",
    allowed: false,
  },
  { type: SSH_AGENTC_LOCK, name: "SSH_AGENTC_LOCK", allowed: false },
  { type: SSH_AGENTC_UNLOCK, name: "SSH_AGENTC_UNLOCK", allowed: false },
  {
    type: SSH_AGENTC_ADD_RSA_ID_CONSTRAINED,
    name: "SSH_AGENTC_ADD_RSA_ID_CONSTRAINED",
    allowed: false,
  },
  { type: SSH_AGENTC_ADD_ID_CONSTRAINED, name: "SSH_AGENTC_ADD_ID_CONSTRAINED", allowed: false },
  {
    type: SSH_AGENTC_ADD_SMARTCARD_KEY_CONSTRAINED,
    name: "SSH_AGENTC_ADD_SMARTCARD_KEY_CONSTRAINED",
    allowed: false,
  },
  { type: SSH_AGENTC_EXTENSION, name: "SSH_AGENTC_EXTENSION", allowed: false },
];

export const SIGN_ONLY_ALLOWLIST = new Set(
  SSH_AGENT_REQUEST_TYPE_VECTORS.filter((v) => v.allowed).map((v) => v.type),
);

function classify(type) {
  return SIGN_ONLY_ALLOWLIST.has(type);
}

/** The exact bytes of a synthesized `SSH_AGENT_FAILURE` reply: a 4-byte
 * length prefix of `1`, followed by the single type byte. Precomputed
 * (rather than built per-call) since it's always identical — the reply
 * never needs to reference which request triggered it. */
export const SSH_AGENT_FAILURE_FRAME = Buffer.from([0, 0, 0, 1, SSH_AGENT_FAILURE]);

/**
 * Thrown by `SignOnlyFilter.feed()` on an oversized declared frame length
 * (see its own doc). Carries `partialResult` — whatever frames were
 * already classified earlier in the SAME `feed()` call, before the
 * oversized frame was reached — same reasoning as
 * src/services/ssh-agent-filter.ts's own SshAgentFrameTooLargeError.
 */
export class SshAgentFrameTooLargeError extends Error {
  constructor(bodyLength, partialResult) {
    super(
      `ssh-agent-filter: frame body length ${bodyLength} exceeds the ` +
        `${MAX_FRAME_BYTES}-byte limit — malformed or hostile input, refusing to continue`,
    );
    this.name = "SshAgentFrameTooLargeError";
    this.bodyLength = bodyLength;
    this.partialResult = partialResult;
  }
}

/**
 * Reassembles length-prefixed SSH-agent-protocol frames out of an
 * arbitrary byte-chunk stream and classifies each complete frame as
 * allowed or blocked per `SIGN_ONLY_ALLOWLIST`. One instance per connection
 * to filter — it holds partial-frame state across calls, so instances must
 * not be shared or reused across unrelated byte streams. Direct port of
 * src/services/ssh-agent-filter.ts's own SignOnlyFilter — see that class's
 * doc comments for the full reasoning behind the growable-buffer shape
 * (not a per-call Buffer.concat, an O(n^2) pitfall) and the copy-not-view
 * returned frames (this instance's own backing buffer is reused and
 * mutated in place by later feed() calls).
 */
export class SignOnlyFilter {
  #buffer = Buffer.alloc(0);
  #length = 0;

  #append(chunk) {
    const needed = this.#length + chunk.length;
    if (needed > this.#buffer.length) {
      const grown = Buffer.allocUnsafe(Math.max(needed, this.#buffer.length * 2, 64));
      this.#buffer.copy(grown, 0, 0, this.#length);
      this.#buffer = grown;
    }
    chunk.copy(this.#buffer, this.#length);
    this.#length += chunk.length;
  }

  #consume(frameLength) {
    this.#buffer.copy(this.#buffer, 0, frameLength, this.#length);
    this.#length -= frameLength;
  }

  /**
   * Feed newly-arrived bytes from an untrusted requester. Returns however
   * many complete frames the accumulated buffer now contains, classified
   * into `{forward, reject, rejectedLengths}`; any trailing partial frame
   * is retained internally for the next call.
   *
   * Throws `SshAgentFrameTooLargeError` if a frame's declared body length
   * exceeds `MAX_FRAME_BYTES` — fail-closed by design, the caller should
   * close the underlying connection on catching it.
   */
  feed(chunk) {
    this.#append(chunk);
    const forward = [];
    const reject = [];
    const rejectedLengths = [];

    for (;;) {
      if (this.#length < LENGTH_PREFIX_BYTES) break;
      const bodyLength = this.#buffer.readUInt32BE(0);
      if (bodyLength > MAX_FRAME_BYTES) {
        throw new SshAgentFrameTooLargeError(bodyLength, { forward, reject, rejectedLengths });
      }
      const frameLength = LENGTH_PREFIX_BYTES + bodyLength;
      if (this.#length < frameLength) break; // incomplete — wait for more bytes

      const frame = Buffer.from(this.#buffer.subarray(0, frameLength));
      this.#consume(frameLength);

      // bodyLength === 0 is malformed (every real message has at least the
      // one type byte) — blocked, not thrown, same reasoning as the TS
      // twin: a zero-length body is fully parsed, bounded input, so
      // there's no reason to tear down the connection over it.
      const type = bodyLength >= 1 ? frame.readUInt8(LENGTH_PREFIX_BYTES) : -1;
      if (classify(type)) {
        forward.push(frame);
      } else {
        reject.push(SSH_AGENT_FAILURE_FRAME);
        rejectedLengths.push(frameLength);
      }
    }

    return { forward, reject, rejectedLengths };
  }
}
