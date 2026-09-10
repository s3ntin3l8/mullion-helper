import { describe, expect, it } from "vitest";
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
