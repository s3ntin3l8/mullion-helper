// Subscribe before requesting a snapshot. An event arriving while the
// snapshot is in flight takes precedence over that potentially stale reply.
export function subscribeWithSnapshot<T>(
  subscribe: (receive: (value: T) => void) => Promise<() => void>,
  snapshot: () => Promise<T>,
  receive: (value: T) => void,
  onError: (error: unknown) => void,
): () => void {
  let cancelled = false;
  let revision = 0;
  let unlisten: (() => void) | undefined;
  void (async () => {
    const stop = await subscribe((value) => {
      if (!cancelled) {
        revision++;
        receive(value);
      }
    });
    if (cancelled) {
      stop();
      return;
    }
    unlisten = stop;
    const before = revision;
    const value = await snapshot();
    if (!cancelled && revision === before) receive(value);
  })().catch((error: unknown) => { if (!cancelled) onError(error); });
  return () => {
    cancelled = true;
    unlisten?.();
  };
}
