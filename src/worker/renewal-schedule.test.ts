import { describe, expect, it, vi, afterEach } from "vitest";
import {
  computeRenewAt,
  isRenewalDue,
  RENEW_AT_FRACTION,
  RENEW_CHECK_INTERVAL_MS,
} from "./renewal-schedule.mjs";

const HOUR = 60 * 60 * 1000;

describe("computeRenewAt", () => {
  it("schedules renewal at half the remaining TTL", () => {
    const now = Date.parse("2026-10-16T00:00:00Z");
    const expiresAt = new Date(now + 24 * HOUR).toISOString();
    expect(computeRenewAt(expiresAt, now)).toBe(now + 12 * HOUR);
    expect(RENEW_AT_FRACTION).toBe(0.5);
  });

  it("renews right away when the expiry is unknown", () => {
    const now = 1_000_000;
    expect(computeRenewAt(undefined, now)).toBe(now);
    expect(computeRenewAt("", now)).toBe(now);
  });

  it("renews right away when the expiry is unparseable, never producing a NaN deadline", () => {
    const now = 1_000_000;
    for (const bad of ["not-a-date", "x", "2026-13-45T99:99:99Z"]) {
      const renewAt = computeRenewAt(bad, now);
      expect(Number.isFinite(renewAt)).toBe(true);
      expect(isRenewalDue(renewAt, now)).toBe(true);
    }
  });

  it("floors an already-expired or near-expiry credential to a one-second delay", () => {
    const now = Date.parse("2026-10-16T00:00:00Z");
    expect(computeRenewAt(new Date(now - HOUR).toISOString(), now)).toBe(
      now + 1_000,
    );
    expect(computeRenewAt(new Date(now + 500).toISOString(), now)).toBe(
      now + 1_000,
    );
  });
});

describe("isRenewalDue", () => {
  afterEach(() => vi.useRealTimers());

  it("counts wall-clock time that passed while the machine was asleep", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-10-15T18:00:00Z"));
    const expiresAt = new Date(Date.now() + 24 * HOUR).toISOString();
    const renewAt = computeRenewAt(expiresAt);
    expect(isRenewalDue(renewAt)).toBe(false);

    // Wall clock jumps 14h (a night asleep) without any timer firing, as a
    // monotonic-clock setTimeout would never notice.
    vi.setSystemTime(new Date("2026-10-16T08:00:00Z"));
    expect(isRenewalDue(renewAt)).toBe(true);
  });

  it("is due exactly at the deadline and not before", () => {
    expect(isRenewalDue(1_000, 999)).toBe(false);
    expect(isRenewalDue(1_000, 1_000)).toBe(true);
  });

  it("polls often enough to react well inside the grace window", () => {
    expect(RENEW_CHECK_INTERVAL_MS).toBeLessThanOrEqual(60_000);
  });
});
