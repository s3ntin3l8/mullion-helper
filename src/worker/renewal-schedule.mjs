// Session-renewal scheduling, kept pure (no timers, no I/O) so the
// wall-clock behavior is directly testable.
//
// Renewal used to be a single `setTimeout(50% of remaining TTL)` armed once
// per worker process. Node timers run on the monotonic clock, which does not
// advance while a laptop sleeps, so a mostly-asleep machine could reach the
// server's wall-clock expiry before that timer fired — and a lapsed session
// can only be recovered by re-pairing. runRun now stores an absolute
// wall-clock `renewAt` and polls it on a short interval instead, which also
// covers wake-from-sleep: the first poll after wake sees the deadline passed.

export const RENEW_AT_FRACTION = 0.5;
export const RENEW_CHECK_INTERVAL_MS = 60_000;
const MIN_RENEW_DELAY_MS = 1_000;

/** Absolute epoch-ms time at which a session expiring at `expiresAt` (ISO
 * string) should be renewed. A missing expiry means "renew right away"
 * rather than guessing a TTL. Floored so an already-near-expiry credential
 * can't produce a zero delay that busy-loops. */
export function computeRenewAt(
  expiresAt,
  now = Date.now(),
  fraction = RENEW_AT_FRACTION,
) {
  if (!expiresAt) return now;
  // An unparseable expiry (the auth reply's expires_at is not validated)
  // must not become a NaN deadline, which would never compare as due and
  // silently disable renewal. Treat it like a missing one: renew now, which
  // re-persists a valid expiry from the server's reply.
  const expiresMs = Date.parse(expiresAt);
  if (!Number.isFinite(expiresMs)) return now;
  const ttlMs = expiresMs - now;
  return now + Math.max(ttlMs * fraction, MIN_RENEW_DELAY_MS);
}

/** Wall-clock comparison — deliberately `now >= renewAt`, not an elapsed-
 * timer count, so time spent asleep counts. */
export function isRenewalDue(renewAt, now = Date.now()) {
  return now >= renewAt;
}
