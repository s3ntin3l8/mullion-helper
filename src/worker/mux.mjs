// Issue #820 (PR6) — a minimal, dependency-free port of
// src/services/ssh-agent-mux.ts's wire protocol for the private worker.
// Deliberately NOT
// imported from src/services/: this file runs standalone on a laptop with
// no Mullion checkout at all, and ssh-agent-mux.ts's createMuxConnection() is written
// against the `ws` package's EventEmitter-style socket
// (`socket.on("message", (data, isBinary) => ...)`), not the WHATWG
// `WebSocket` builtin this file uses instead — the two aren't just
// differently-typed, they're differently-shaped at runtime, so importing
// across that boundary wouldn't work even with types erased. Both sides of
// this wire protocol are deliberately maintained as two separate
// implementations; a wire-format change to ssh-agent-mux.ts's FrameType/
// HEADER_BYTES layout must be mirrored here by hand — grep this repo for
// "ssh-agent-mux.ts" if you're touching one and forget the other exists.
//
// Scope is deliberately narrower than the full MuxConnection: this side is
// always "odd" parity (ssh-agent-mux.ts's own channelIdParity doc — the WS
// *client* is odd, and the helper always dials OUT to /ws/agent-bridge) and
// never initiates its own channel opens — the primary is the one fanning
// channels out to a live bridge (ssh-agent-fanout.ts's onChannel handler
// calling bridge.mux.openChannel(), "even" parity there), never the
// reverse. So this file only implements the inbound half: accept a
// peer-initiated Open, OpenAck it, hand the caller a channel-like object.
// There is no id allocator here at all — nothing to collide, since this
// side never assigns a channel id of its own.

const HEADER_BYTES = 5;

export const FrameType = Object.freeze({
  Open: 1,
  OpenAck: 2,
  OpenFail: 3,
  Data: 4,
  WindowAdjust: 5,
  Eof: 6,
  Close: 7,
  Ping: 8,
  Pong: 9,
});

// Mirrors CHANNEL_WINDOW_BYTES / WINDOW_ADJUST_THRESHOLD_BYTES in
// src/services/ssh-agent-mux.ts exactly — see that file's own doc comment
// for why these values were chosen. A mismatch here wouldn't break
// correctness (each side clamps to what IT was granted), just make this
// side needlessly send WINDOW_ADJUST more or less often than intended.
// Issue #1059 — this value is also pinned in
// ssh-agent-protocol-v1.json (`muxTransport.channelWindowBytes`), and
// mux.test.ts asserts this export matches the fixture.
export const CHANNEL_WINDOW_BYTES = 256 * 1024;
const WINDOW_ADJUST_THRESHOLD_BYTES = CHANNEL_WINDOW_BYTES / 2;

// Mirrors DEFAULT_MAX_CHANNELS in src/services/ssh-agent-mux.ts (Hermes
// review, PR #866) — that module refuses (OpenFail) a peer Open once its
// tracked channel count reaches this cap, citing it as load-bearing
// memory-hardening against a peer that opens and abandons channels
// without limit. This port accepted every inbound Open unconditionally
// until this fix, with no bound on `channels` — reachability is narrow
// (the only peer here is the user's own paired primary, itself capped at
// the same 256), but this file otherwise deliberately mirrors every one
// of the source's hardening invariants (handleWindowAdjust's clamp,
// decodeFrame never throwing), so this cap belongs here for the same
// reason those do.
const DEFAULT_MAX_CHANNELS = 256;

export function encodeHeader(type, channelId) {
  const buf = Buffer.allocUnsafe(HEADER_BYTES);
  buf.writeUInt8(type, 0);
  buf.writeUInt32BE(channelId, 1);
  return buf;
}

function encodeWindowAdjust(channelId, byteCount) {
  const buf = Buffer.allocUnsafe(HEADER_BYTES + 4);
  buf.writeUInt8(FrameType.WindowAdjust, 0);
  buf.writeUInt32BE(channelId, 1);
  buf.writeUInt32BE(byteCount, 5);
  return buf;
}

function encodeData(channelId, payload) {
  return Buffer.concat([encodeHeader(FrameType.Data, channelId), payload]);
}

/** Returns `null` for anything too short or of an unrecognized type — a
 * malformed frame is dropped, never thrown, for the same reason
 * ssh-agent-mux.ts's decodeFrame() never throws: this runs synchronously
 * inside the WebSocket's own "message" dispatch. */
export function decodeFrame(raw) {
  if (raw.length < HEADER_BYTES) return null;
  const type = raw.readUInt8(0);
  const channelId = raw.readUInt32BE(1);
  switch (type) {
    case FrameType.Open:
    case FrameType.OpenAck:
    case FrameType.OpenFail:
    case FrameType.Eof:
    case FrameType.Close:
    case FrameType.Ping:
    case FrameType.Pong:
      return { type, channelId };
    case FrameType.WindowAdjust:
      if (raw.length < HEADER_BYTES + 4) return null;
      return { type, channelId, byteCount: raw.readUInt32BE(HEADER_BYTES) };
    case FrameType.Data:
      return { type, channelId, payload: raw.subarray(HEADER_BYTES) };
    default:
      return null;
  }
}

function invokeListener(listener, ...args) {
  try {
    listener(...args);
  } catch {
    // Intentionally swallowed — mirrors ssh-agent-mux.ts's own
    // invokeListener(): this runs synchronously inside the WebSocket's
    // "message" handler, and an uncaught throw here would take down this
    // process's one and only connection instead of just this one channel.
  }
}

class InboundChannel {
  #sendWindowBytes = CHANNEL_WINDOW_BYTES;
  #unacknowledgedBytes = 0;
  #dataListeners = [];
  #eofListeners = [];
  #closeListeners = [];
  #drainListeners = [];
  #sendFrame;
  #onLocalClose;
  closed = false;

  constructor(id, sendFrame, onLocalClose) {
    this.id = id;
    this.#sendFrame = sendFrame;
    this.#onLocalClose = onLocalClose;
  }

  get sendWindow() {
    return this.#sendWindowBytes;
  }

  send(chunk) {
    if (this.closed) throw new Error(`ssh-agent-bridge-mux: send() on closed channel ${this.id}`);
    if (chunk.length > this.#sendWindowBytes) {
      throw new Error(
        `ssh-agent-bridge-mux: send() of ${chunk.length} bytes exceeds channel ${this.id}'s ` +
          `remaining window of ${this.#sendWindowBytes} — caller must respect sendWindow/onDrain`,
      );
    }
    this.#sendWindowBytes -= chunk.length;
    this.#sendFrame(encodeData(this.id, chunk));
  }

  eof() {
    if (this.closed) return;
    this.#sendFrame(encodeHeader(FrameType.Eof, this.id));
  }

  close() {
    if (this.closed) return;
    this.closed = true;
    this.#sendFrame(encodeHeader(FrameType.Close, this.id));
    this.#onLocalClose(this.id);
    for (const listener of this.#closeListeners) invokeListener(listener);
  }

  /** Local-close on connection teardown — does not send a Close frame
   * (nowhere left to send it). */
  closeLocally() {
    if (this.closed) return;
    this.closed = true;
    for (const listener of this.#closeListeners) invokeListener(listener);
  }

  onData(listener) {
    this.#dataListeners.push(listener);
  }
  onEof(listener) {
    this.#eofListeners.push(listener);
  }
  onClose(listener) {
    this.#closeListeners.push(listener);
  }
  onDrain(listener) {
    this.#drainListeners.push(listener);
  }

  handleData(chunk) {
    for (const listener of this.#dataListeners) invokeListener(listener, chunk);
  }
  handleEof() {
    for (const listener of this.#eofListeners) invokeListener(listener);
  }
  handleWindowAdjust(byteCount) {
    // Clamped to CHANNEL_WINDOW_BYTES, not accepted unchecked — mirrors
    // ssh-agent-mux.ts's own handleWindowAdjust() and the Hermes finding
    // (PR #853) it fixed: an attacker-controlled byteCount must not be
    // able to inflate this side's belief about how much it may send.
    this.#sendWindowBytes = Math.min(this.#sendWindowBytes + byteCount, CHANNEL_WINDOW_BYTES);
    for (const listener of this.#drainListeners) invokeListener(listener);
  }

  acknowledgeConsumed(byteCount) {
    if (this.closed) return;
    this.#unacknowledgedBytes += byteCount;
    if (this.#unacknowledgedBytes >= WINDOW_ADJUST_THRESHOLD_BYTES) {
      const toAcknowledge = this.#unacknowledgedBytes;
      this.#unacknowledgedBytes = 0;
      this.#sendFrame(encodeWindowAdjust(this.id, toAcknowledge));
    }
  }
}

/**
 * Wires a WHATWG `WebSocket` (already open, `binaryType` will be forced to
 * "arraybuffer" here) as the inbound-only half of the mux protocol.
 * `opts.onChannel(channel)` fires once per peer-initiated Open this side
 * accepts, with an OpenAck already sent. Automatically answers Ping with
 * Pong (ssh-agent-mux.ts's own liveness contract — the primary tears the
 * connection down if this side ever stops answering). Returns a handle with
 * `close()` (tears down every open channel + the socket) and `onClose(cb)`
 * (fires once, when the socket closes for any reason).
 */
export function attachInboundMux(ws, opts) {
  const channels = new Map();
  const closeListeners = [];
  let closed = false;

  ws.binaryType = "arraybuffer";

  function sendFrame(frame) {
    if (ws.readyState !== WebSocket.OPEN) return;
    ws.send(frame);
  }

  function removeChannel(id) {
    channels.delete(id);
  }

  function teardown() {
    if (closed) return;
    closed = true;
    for (const channel of channels.values()) channel.closeLocally();
    channels.clear();
    for (const listener of closeListeners) invokeListener(listener);
  }

  ws.addEventListener("message", (event) => {
    if (typeof event.data === "string") return; // binary-only protocol
    const frame = decodeFrame(Buffer.from(event.data));
    if (frame === null) return;

    if (frame.type === FrameType.Ping) {
      sendFrame(encodeHeader(FrameType.Pong, 0));
      return;
    }
    if (frame.type === FrameType.Pong) return; // this side never pings first

    if (frame.type === FrameType.Open) {
      if (channels.has(frame.channelId) || channels.size >= DEFAULT_MAX_CHANNELS) {
        sendFrame(encodeHeader(FrameType.OpenFail, frame.channelId));
        return;
      }
      const channel = new InboundChannel(frame.channelId, sendFrame, removeChannel);
      channels.set(frame.channelId, channel);
      sendFrame(encodeHeader(FrameType.OpenAck, frame.channelId));
      invokeListener(opts.onChannel, channel);
      return;
    }

    // OpenAck/OpenFail are unreachable here — this side never calls
    // openChannel() (see this file's own header comment) — so any peer
    // that sends one anyway is disregarded, matching ssh-agent-mux.ts's own
    // "unknown/already-resolved id — ignore" posture for the analogous
    // case rather than treating it as an error worth tearing the
    // connection down over.

    const channel = channels.get(frame.channelId);
    if (channel === undefined) return;
    switch (frame.type) {
      case FrameType.Data:
        channel.handleData(frame.payload);
        return;
      case FrameType.WindowAdjust:
        channel.handleWindowAdjust(frame.byteCount);
        return;
      case FrameType.Eof:
        channel.handleEof();
        return;
      case FrameType.Close:
        channels.delete(frame.channelId);
        channel.closeLocally();
        return;
    }
  });

  ws.addEventListener("close", teardown);
  ws.addEventListener("error", teardown);

  return {
    onClose(listener) {
      closeListeners.push(listener);
    },
    close() {
      if (closed) return;
      for (const channel of channels.values()) channel.close();
      if (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING) {
        ws.close();
      }
      teardown();
    },
  };
}

/**
 * Pipes an accepted `net.Socket` (this helper's own connection to the
 * laptop's real SSH agent, dialed via `net.connect({path: SSH_AUTH_SOCK})`)
 * through a mux channel, with the same two-way, real backpressure as
 * ssh-agent-mux.ts's own pipeNetSocketToChannel() — see that function's
 * doc comment for the full reasoning; this is a direct, deliberate port,
 * not a reduced approximation, since the flow-control invariants it
 * protects (never buffer unboundedly, never silently drop a chunk) matter
 * exactly as much on this end of the wire as the other.
 */
/**
 * The socket->channel half of `pipeNetSocketToChannel` below — the real
 * agent's own REPLIES flowing back out. Split out (round 4 PR2, issue
 * #820) so `filtered-relay.mjs` can compose this UNFILTERED half
 * unchanged with its own filtered request-direction half — replies are
 * never filtered (see filter.mjs's own header comment on why).
 * Kept in this file, not the relay module: this is still pure mux-frame
 * plumbing, no agent-protocol awareness, matching this file's own role as
 * transport only (mirrors src/services/ssh-agent-mux.ts's own
 * pipeChannelDirection/pipeNetSocketToChannel split, which
 * src/services/ssh-agent-relay.ts composes from the same way).
 */
export function pipeSocketRepliesToChannel(socket, channel) {
  let socketEnded = false;
  let pendingChunk = null;

  function flushPending() {
    if (pendingChunk === null) return;
    if (channel.closed || socketEnded) {
      pendingChunk = null;
      return;
    }
    if (pendingChunk.length > channel.sendWindow) return;
    const chunk = pendingChunk;
    pendingChunk = null;
    channel.send(chunk);
    socket.resume();
  }
  channel.onDrain(flushPending);

  socket.on("data", (chunk) => {
    if (channel.closed) return;
    if (pendingChunk !== null) {
      pendingChunk = null;
      if (!channel.closed) channel.close();
      if (!socket.destroyed) socket.destroy();
      return;
    }
    if (chunk.length > channel.sendWindow) {
      socket.pause();
      pendingChunk = chunk;
      return;
    }
    channel.send(chunk);
  });
  socket.on("end", () => {
    socketEnded = true;
    if (!channel.closed) channel.eof();
  });
  socket.on("close", () => {
    if (!channel.closed) channel.close();
  });
  socket.on("error", () => {
    if (!channel.closed) channel.close();
  });
}

/**
 * The channel->socket half of `pipeNetSocketToChannel` below — REQUESTS
 * flowing in toward the real agent, unfiltered. Split out for the same
 * reason as `pipeSocketRepliesToChannel` above; unlike that half, this one
 * is NOT reused unfiltered by `filtered-relay.mjs` — that module
 * reimplements this direction itself with `SignOnlyFilter` inserted, since
 * the two need different framing (filtered request handling has to
 * classify each agent-protocol frame, not just forward raw byte chunks).
 * Kept here anyway, alongside its sibling, as the reference for what
 * "unfiltered" looks like and for any future direct (non-agent, e.g. a
 * test double) caller that genuinely wants a raw pipe.
 */
export function pipeChannelRequestsToSocket(socket, channel) {
  channel.onData((chunk) => {
    if (socket.destroyed) return;
    const byteLength = chunk.length;
    socket.write(chunk, () => channel.acknowledgeConsumed(byteLength));
  });
  channel.onEof(() => {
    if (!socket.destroyed) socket.end();
  });
  channel.onClose(() => {
    if (!socket.destroyed) socket.destroy();
  });
}

export function pipeNetSocketToChannel(socket, channel) {
  pipeSocketRepliesToChannel(socket, channel);
  pipeChannelRequestsToSocket(socket, channel);
}
