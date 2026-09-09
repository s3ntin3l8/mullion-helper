// Issue #820 (PR6) — deliberately duplicated from
// src/services/bridge-registry.ts's decodePairingPayload(), not imported:
// that module pulls in drizzle-orm and the DB schema, neither of which
// belong in a standalone laptop CLI (see ssh-agent-bridge-mux.mjs's own
// header comment for the general "why duplicated, not shared" reasoning).
// The format itself — plain base64url of `{baseUrl, code}` — is stable and
// intentionally trivial precisely so two independent implementations can't
// drift on it; if it ever needs to change, bridge-registry.ts's own
// encodePairingPayload() is the side that changes first, and this must be
// updated to match by hand.

/** A paste from Settings is at most one short line of base64url — there's
 * no legitimate payload anywhere near this size, and a very large string
 * would otherwise be decoded to a multi-MB UTF-8 buffer and parsed by
 * `JSON.parse` (Issue #1055). 8 KiB is generous for `{ baseUrl, code }`
 * (both short strings) and small enough to make the attack cost
 * meaningless. */
const MAX_PAIRING_PAYLOAD_BYTES = 8 * 1024;

/** Returns `null` for anything that isn't a well-formed payload — a
 * mistyped/truncated paste is this CLI's own input-validation concern, not
 * something to throw over. */
export function decodePairingPayload(encoded) {
  if (typeof encoded !== "string" || encoded.length > MAX_PAIRING_PAYLOAD_BYTES) {
    return null;
  }
  let parsed;
  try {
    parsed = JSON.parse(Buffer.from(encoded, "base64url").toString("utf8"));
  } catch {
    return null;
  }
  if (typeof parsed !== "object" || parsed === null) return null;
  const { baseUrl, code } = parsed;
  if (typeof baseUrl !== "string" || typeof code !== "string") return null;
  if (baseUrl === "" || code === "") return null;
  if (!isValidHttpUrl(baseUrl)) return null;
  return { baseUrl, code };
}

/** Mirrors ssh-agent-bridge-helper.mjs's own `isValidHttpBaseUrl` — kept
 * local here so this module has no other source dependency. If either
 * side ever changes the rule, the other must be updated by hand (see the
 * file header above). */
function isValidHttpUrl(value) {
  try {
    const url = new URL(value);
    return url.protocol === "http:" || url.protocol === "https:";
  } catch {
    return false;
  }
}
