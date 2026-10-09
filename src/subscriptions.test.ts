import { describe, expect, it, vi } from "vitest";
import { subscribeWithSnapshot } from "./subscriptions";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => { resolve = complete; });
  return { promise, resolve };
}

describe("subscribeWithSnapshot", () => {
  it("registers the listener before reading and keeps an event newer than the snapshot", async () => {
    const snapshot = deferred<string>();
    const receive = vi.fn();
    let event!: (value: string) => void;
    const read = vi.fn(() => snapshot.promise);
    const unsubscribe = vi.fn();
    const stop = subscribeWithSnapshot(async (listener) => {
      expect(read).not.toHaveBeenCalled();
      event = listener;
      return unsubscribe;
    }, read, receive, vi.fn());
    await Promise.resolve();
    expect(read).toHaveBeenCalledOnce();
    event("connected");
    snapshot.resolve("starting");
    await Promise.resolve();
    expect(receive.mock.calls).toEqual([["connected"]]);
    stop();
    expect(unsubscribe).toHaveBeenCalledOnce();
  });

  it("cleans up a listener that finishes registering after unmount", async () => {
    const registration = deferred<() => void>();
    const read = vi.fn(async () => "cached");
    const receive = vi.fn();
    const unsubscribe = vi.fn();
    const stop = subscribeWithSnapshot(() => registration.promise, read, receive, vi.fn());
    stop();
    registration.resolve(unsubscribe);
    await Promise.resolve();
    expect(unsubscribe).toHaveBeenCalledOnce();
    expect(read).not.toHaveBeenCalled();
    expect(receive).not.toHaveBeenCalled();
  });

  it("ignores in-flight snapshots and events after unmount", async () => {
    const snapshot = deferred<string>();
    const receive = vi.fn();
    let event!: (value: string) => void;
    const stop = subscribeWithSnapshot(async (listener) => {
      event = listener;
      return () => {};
    }, () => snapshot.promise, receive, vi.fn());
    await Promise.resolve();
    stop();
    event("available");
    snapshot.resolve("cached");
    await Promise.resolve();
    expect(receive).not.toHaveBeenCalled();
  });

  it("loads a cached value when there is no intervening event", async () => {
    const receive = vi.fn();
    const stop = subscribeWithSnapshot(async () => () => {}, async () => "available", receive, vi.fn());
    await Promise.resolve();
    await Promise.resolve();
    expect(receive).toHaveBeenCalledWith("available");
    stop();
  });
});
