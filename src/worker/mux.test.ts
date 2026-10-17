import { afterEach, describe, expect, it, vi } from "vitest";
import { attachInboundMux, decodeFrame, encodeHeader, FrameType } from "./mux.mjs";

describe("bridge mux protocol", () => {
  it("encodes and decodes the five-byte protocol header", () => {
    const header = encodeHeader(FrameType.Data, 42);
    expect([...header]).toEqual([FrameType.Data, 0, 0, 0, 42]);
    expect(decodeFrame(Buffer.concat([header, Buffer.from("payload")]))).toMatchObject({ type: FrameType.Data, channelId: 42 });
  });

  it("answers a server ping with a channel-zero pong", () => {
    const sent: Buffer[] = [];
    const listeners = new Map<string, Array<(event: { data: ArrayBuffer }) => void>>();
    const socket = {
      readyState: WebSocket.OPEN,
      binaryType: "blob",
      addEventListener(name: string, listener: (event: { data: ArrayBuffer }) => void) { listeners.set(name, [...(listeners.get(name) ?? []), listener]); },
      send(frame: Buffer) { sent.push(frame); }, close() {},
    };
    attachInboundMux(socket, { onChannel() {} });
    const ping = encodeHeader(FrameType.Ping, 0);
    const data = ping.buffer.slice(ping.byteOffset, ping.byteOffset + ping.byteLength) as ArrayBuffer;
    for (const listener of listeners.get("message") ?? []) listener({ data });
    expect(sent).toHaveLength(1);
    expect(decodeFrame(sent[0])).toMatchObject({ type: FrameType.Pong, channelId: 0 });
  });
});

describe("read-idle timeout", () => {
  afterEach(() => { vi.useRealTimers(); });

  function fakeSocket() {
    const listeners = new Map<string, Array<(event: { data: ArrayBuffer }) => void>>();
    const socket = {
      readyState: WebSocket.OPEN,
      binaryType: "blob",
      closeCalls: 0,
      addEventListener(name: string, listener: (event: { data: ArrayBuffer }) => void) { listeners.set(name, [...(listeners.get(name) ?? []), listener]); },
      send() {},
      close() { socket.closeCalls++; },
    };
    const emitPing = () => {
      const ping = encodeHeader(FrameType.Ping, 0);
      const data = ping.buffer.slice(ping.byteOffset, ping.byteOffset + ping.byteLength) as ArrayBuffer;
      for (const listener of listeners.get("message") ?? []) listener({ data });
    };
    return { socket, emitPing, emit: (name: string) => { for (const l of listeners.get(name) ?? []) l({ data: new ArrayBuffer(0) }); } };
  }

  it("closes the socket and fires onClose when nothing arrives within the timeout", () => {
    vi.useFakeTimers();
    const { socket } = fakeSocket();
    const handle = attachInboundMux(socket, { onChannel() {}, idleTimeoutMs: 1000 });
    let closed = 0;
    handle.onClose(() => { closed++; });
    vi.advanceTimersByTime(999);
    expect(closed).toBe(0);
    vi.advanceTimersByTime(1);
    expect(socket.closeCalls).toBe(1);
    expect(closed).toBe(1);
  });

  it("stays open while messages keep arriving", () => {
    vi.useFakeTimers();
    const { socket, emitPing } = fakeSocket();
    const handle = attachInboundMux(socket, { onChannel() {}, idleTimeoutMs: 1000 });
    let closed = 0;
    handle.onClose(() => { closed++; });
    for (let i = 0; i < 5; i++) { vi.advanceTimersByTime(800); emitPing(); }
    expect(closed).toBe(0);
    expect(socket.closeCalls).toBe(0);
    vi.advanceTimersByTime(1000);
    expect(closed).toBe(1);
  });

  it("defaults to 50s (not a multiple of the 15s ping interval)", () => {
    vi.useFakeTimers();
    const { socket } = fakeSocket();
    attachInboundMux(socket, { onChannel() {} });
    vi.advanceTimersByTime(49_999);
    expect(socket.closeCalls).toBe(0);
    vi.advanceTimersByTime(1);
    expect(socket.closeCalls).toBe(1);
  });

  it("clears the timer once the socket closes", () => {
    vi.useFakeTimers();
    const { socket, emit } = fakeSocket();
    attachInboundMux(socket, { onChannel() {}, idleTimeoutMs: 1000 });
    emit("close");
    expect(vi.getTimerCount()).toBe(0);
    vi.advanceTimersByTime(5000);
    expect(socket.closeCalls).toBe(0);
  });
});
