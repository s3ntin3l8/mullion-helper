# Bundled bridge worker

This private sidecar is the laptop half of Mullion's SSH-agent bridge. Its protocol, reconnect/renewal behavior, mux, and sign-only filter were extracted from `s3ntin3l8/mullion-session-manager` at commit `72fff2bacc0c`.

The desktop app is the only supported process owner. The worker intentionally exposes only `pair`, `run`, `inspect`, and `version`; legacy service installation and detached execution do not live here. Pairing payloads arrive on stdin so they are not exposed in process arguments, and `inspect` never emits the saved session token.

`ssh-agent-protocol-v1.json` is the versioned compatibility boundary shared with the primary. Changes to bridge framing, mux window size, or the request allowlist must update the fixture and be tested in both repositories.

With `run --json-events`, stdout is newline-delimited JSON for the desktop
supervisor. Events are `connected` (`bridge_id`, `base_url`), `disconnected`,
`connect_failed` (`message`), `session_renewed` (`expires_at`), `renewal_retry`
(`delay_ms`, `message`), `renewal_rejected`, and `dead_credential` (`message`).
Human-readable diagnostics remain on stderr.

If no inbound mux frame (data or ping) arrives for `DEFAULT_IDLE_TIMEOUT_MS`
(50s, about three of the primary's 15s pings), the connection is treated as
half-open: it is closed locally, `disconnected` is emitted, and the reconnect
loop resumes. A successful renewal reply received while the worker is being
stopped by SIGINT/SIGTERM is still persisted; a hard kill (as the desktop
supervisor does) relies on the primary keeping the previous session id valid.
