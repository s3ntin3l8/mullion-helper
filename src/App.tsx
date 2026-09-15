import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { api } from "./api";
import type { AgentCandidate, BridgeStatus, Notice, Settings } from "./types";

const CUSTOM_PATH = "__custom__";

// `chosenPath` comes straight from the backend's `list_agent_sockets`
// command, which computes it with the same `choose_best` `resolve_agent`
// uses -- this only looks up that candidate's label for display, it never
// re-derives the ranking, so the "Auto-detect (...)" text can't drift from
// what auto-detect actually picks.
function describeAutoDetect(candidates: AgentCandidate[], chosenPath: string | null): string {
  const winner = chosenPath == null ? undefined : candidates.find((candidate) => candidate.path === chosenPath);
  if (!winner) return "Auto-detect (no agent found)";
  const identities = winner.identities == null ? "connected" : `${winner.identities} ${winner.identities === 1 ? "identity" : "identities"}`;
  return `Auto-detect (${winner.label} — ${identities})`;
}

function describeCandidate(candidate: AgentCandidate): string {
  if (!candidate.reachable) return `${candidate.label} — not reachable`;
  if (candidate.identities == null) return `${candidate.label} — connected`;
  return `${candidate.label} — ${candidate.identities} ${candidate.identities === 1 ? "identity" : "identities"}`;
}

function errorNotice(raw: string): Notice {
  const text = raw.trim();
  const breakAt = text.indexOf("\n");
  return breakAt === -1
    ? { kind: "error", summary: text }
    : { kind: "error", summary: text.slice(0, breakAt).trim(), details: text.slice(breakAt + 1).trim() };
}
function infoNotice(summary: string): Notice {
  return { kind: "info", summary };
}

function BrandMark() {
  return <svg className="brand-mark" viewBox="0 0 32 32" aria-hidden="true">
    <rect fill="#11211a" x="1" y="1" width="13.5" height="13.5" rx="2" />
    <rect fill="#11211a" x="17.5" y="1" width="13.5" height="13.5" rx="2" />
    <rect fill="#11211a" x="1" y="17.5" width="13.5" height="13.5" rx="2" />
    <rect fill="#0e9f6e" x="17.5" y="17.5" width="13.5" height="13.5" rx="2" />
  </svg>;
}

const labels: Record<BridgeStatus["state"], string> = {
  unpaired: "Ready to pair", starting: "Starting bridge…", connected: "Bridge connected",
  reconnecting: "Reconnecting…", paused: "Bridge paused", needs_pairing: "Pairing required",
  agent_unavailable: "SSH agent unavailable", error: "Bridge needs attention",
};

export function App() {
  const [status, setStatus] = useState<BridgeStatus | null>(null);
  const [settings, setSettings] = useState<Settings | null>(null);
  const [payload, setPayload] = useState("");
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [confirmingUnpair, setConfirmingUnpair] = useState(false);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const [diagnosticsPath, setDiagnosticsPath] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [version, setVersion] = useState<string | null>(null);
  const [appName, setAppName] = useState("Mullion Helper");
  const [agentCandidates, setAgentCandidates] = useState<AgentCandidate[]>([]);
  const [autoDetectChosenPath, setAutoDetectChosenPath] = useState<string | null>(null);
  const [manualCustomSocket, setManualCustomSocket] = useState(false);
  const loadAgentCandidates = useCallback(async () => {
    const result = await api.listAgentSockets();
    setAgentCandidates(result.candidates);
    setAutoDetectChosenPath(result.chosen);
  }, []);
  const refresh = useCallback(async () => {
    const [nextStatus, nextSettings] = await Promise.all([api.status(), api.settings()]);
    if (api.isDesktop) nextSettings.launch_at_login = await isEnabled();
    setStatus(nextStatus); setSettings(nextSettings);
  }, []);

  useEffect(() => {
    void refresh().catch((error: unknown) => setNotice(errorNotice(String(error))));
    if (!api.isDesktop) return;
    let unlisten = () => {};
    void listen<BridgeStatus>("bridge-status", (event) => setStatus(event.payload)).then((fn) => { unlisten = fn; });
    return () => unlisten();
  }, [refresh]);

  useEffect(() => {
    void api.diagnosticsPath().then(setDiagnosticsPath).catch(() => {});
  }, []);

  useEffect(() => {
    void api.version().then(setVersion).catch(() => {});
  }, []);

  useEffect(() => {
    void api.appName().then(setAppName).catch(() => {});
  }, []);

  useEffect(() => {
    void loadAgentCandidates().catch(() => {});
  }, [loadAgentCandidates]);

  useEffect(() => {
    if (!api.isDesktop) return;
    const check = () => void api.checkForUpdates().then((result) => {
      if (result.available) setUpdateVersion(result.version);
    }).catch(() => {});
    const initial = window.setTimeout(check, 3000);
    const daily = window.setInterval(check, 24 * 60 * 60 * 1000);
    return () => { window.clearTimeout(initial); window.clearInterval(daily); };
  }, []);

  async function copyDetails(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch { /* clipboard unavailable; the text is still selectable in the details block */ }
  }
  async function act(operation: () => Promise<BridgeStatus>) {
    setBusy(true); setNotice(null);
    try { setStatus(await operation()); } catch (error) { setNotice(errorNotice(String(error))); }
    finally { setBusy(false); }
  }
  async function pair() {
    await act(async () => {
      const paired = await api.pair(payload.trim());
      if (api.isDesktop && !(await isEnabled())) await enable();
      await refresh();
      setPayload(""); return paired;
    });
  }
  async function unpair() {
    await act(async () => {
      try {
        const result = await api.unpair();
        await refresh();
        return result;
      } catch (error) {
        // The backend has already set desired=false and stopped the child
        // by the time unpair() can throw, no matter which step inside it
        // failed -- so the card showing the pre-unpair status (e.g. still
        // "Bridge connected") is stale, not just pending. Refresh so it
        // reflects what the backend now reports (likely Error, with the
        // failure in detail) instead of a status that's already wrong.
        // Swallow refresh's own failure so it can't replace the error
        // act() is about to show as the notice.
        await refresh().catch(() => {});
        throw error;
      } finally {
        // Reset the confirm row on rejection too -- otherwise it stays
        // expanded over a machine that may already be unpaired (act()'s
        // catch only sets an error notice, it doesn't touch this state).
        setConfirmingUnpair(false);
      }
    });
  }
  async function save() {
    if (!settings) return;
    setBusy(true); setNotice(null);
    try {
      if (api.isDesktop) {
        if (settings.launch_at_login) await enable(); else await disable();
      }
      setSettings(await api.saveSettings(settings));
      // While unpaired there's no running worker for save_settings to
      // restart -- saying so anyway would be actively wrong, not just vague.
      setNotice(infoNotice(needsPairing ? "Settings saved." : "Settings saved. The bridge was restarted with the new configuration."));
    } catch (error) { setNotice(errorNotice(String(error))); } finally { setBusy(false); }
  }
  async function checkUpdates() {
    setBusy(true);
    try {
      const result = await api.checkForUpdates();
      setUpdateVersion(result.available ? result.version : null);
      setNotice(infoNotice(result.available ? `Version ${result.version} is available.` : "Mullion Helper is up to date."));
    } catch (error) { setNotice(errorNotice(String(error))); } finally { setBusy(false); }
  }

  const connected = status?.state === "connected";
  const running = status && !["paused", "unpaired", "needs_pairing", "error"].includes(status.state);
  const needsPairing = status && ["unpaired", "needs_pairing"].includes(status.state);
  // A stored path outside the detected list (e.g. a headless setup, or a
  // candidate that just isn't reachable right now) must still render as
  // itself rather than silently reverting to "Auto-detect" -- so custom
  // mode is shown whenever the stored value doesn't match a known
  // candidate, in addition to whenever the user explicitly picked it from
  // the dropdown this session.
  const knownAgentPaths = agentCandidates.map((candidate) => candidate.path);
  const storedSocketIsCustom = !!settings?.ssh_auth_sock.trim() && !knownAgentPaths.includes(settings.ssh_auth_sock);
  const showCustomSocketInput = manualCustomSocket || storedSocketIsCustom;
  const agentSocketSelectValue = showCustomSocketInput ? CUSTOM_PATH : (settings?.ssh_auth_sock ?? "");
  // Reused for both first-time setup (shown expanded, below) and re-pairing
  // an already-paired computer (shown collapsed inside Settings) — same
  // payload state and the same pair() handler either way.
  const pairingForm = <>
    <label>Pairing payload<textarea value={payload} onChange={(event) => setPayload(event.target.value)} placeholder="Paste pairing payload" rows={3} /></label>
    <button disabled={busy || !payload.trim()} onClick={() => void pair()}>{busy ? "Pairing…" : "Pair and start"}</button>
  </>;
  return <main className="shell">
    <header><BrandMark /><div><h1>Mullion Helper</h1><p>Your local SSH-agent bridge</p></div></header>
    <section className="status-card" aria-live="polite">
      <span className={`orb ${status?.state ?? "starting"}`} />
      <div className="status-copy"><strong>{status ? labels[status.state] : "Loading…"}</strong><span>{status?.detail ?? (connected ? "Your SSH agent is available to Mullion sessions." : "The tray icon keeps the bridge available in the background.")}</span>{status?.base_url && <small>{status.base_url}</small>}</div>
      {!needsPairing && (running ? <button className="secondary" disabled={busy} onClick={() => void act(api.pause)}>Pause</button> : <button disabled={busy} onClick={() => void act(api.start)}>Start</button>)}
    </section>
    {connected && status?.agent_identities === 0 && <p className="agent-warning">Your SSH agent is connected but has no identities loaded — unlock it, or check Settings → SSH agent socket.</p>}
    {needsPairing && <section className="panel onboarding"><span className="eyebrow">First-time setup</span><h2>Connect this computer</h2><p>In Mullion, open Settings → Hosts → SSH agent bridges, create a pairing code, then paste the payload below.</p>{pairingForm}</section>}
    {settings && <section className="panel"><span className="eyebrow">Configuration</span><h2>Settings</h2>
      <label>SSH agent socket
        <div className="agent-socket-row">
          <select
            value={agentSocketSelectValue}
            onChange={(event) => {
              const value = event.target.value;
              if (value === CUSTOM_PATH) {
                setManualCustomSocket(true);
              } else {
                setManualCustomSocket(false);
                setSettings({ ...settings, ssh_auth_sock: value });
              }
            }}
          >
            <option value="">{describeAutoDetect(agentCandidates, autoDetectChosenPath)}</option>
            {agentCandidates.map((candidate) => <option key={candidate.path} value={candidate.path}>{describeCandidate(candidate)}</option>)}
            <option value={CUSTOM_PATH}>Custom path…</option>
          </select>
          <button type="button" className="secondary" disabled={busy} onClick={() => void loadAgentCandidates()}>Re-detect</button>
        </div>
      </label>
      {showCustomSocketInput && <label>Custom socket path<input value={settings.ssh_auth_sock} onChange={(event) => setSettings({ ...settings, ssh_auth_sock: event.target.value })} placeholder="/path/to/agent.sock" /></label>}
      <p className="hint">Leave on Auto-detect to use SSH_AUTH_SOCK, 1Password, or the Windows OpenSSH-compatible pipe.</p><label className="toggle"><input type="checkbox" checked={settings.launch_at_login} onChange={(event) => setSettings({ ...settings, launch_at_login: event.target.checked })} /><span>Launch at login</span></label><label className="toggle"><input type="checkbox" checked={settings.insecure} onChange={(event) => setSettings({ ...settings, insecure: event.target.checked })} /><span>Allow self-signed TLS certificates</span></label><button disabled={busy} onClick={() => void save()}>Save settings</button>
      {!needsPairing && <div className="repair">
        <details><summary>Re-pair this computer</summary><div className="payload-form">{pairingForm}</div></details>
        <div className="unpair-row">{confirmingUnpair
          ? <span className="confirm">Remove this computer's pairing?<button type="button" className="button-danger" disabled={busy} onClick={() => void unpair()}>Unpair</button><button type="button" className="link" disabled={busy} onClick={() => setConfirmingUnpair(false)}>Cancel</button></span>
          : <button type="button" className="link danger" onClick={() => setConfirmingUnpair(true)}>Unpair this computer</button>}</div>
      </div>}
    </section>}
    {updateVersion && <section className="update"><div><strong>Mullion Helper {updateVersion} is available</strong><span>The app will restart after installing.</span></div><button disabled={busy} onClick={() => { setBusy(true); void api.installUpdate().catch((error) => { setNotice(errorNotice(String(error))); setBusy(false); }); }}>Install update</button></section>}
    {notice && (notice.kind === "error"
      ? <section className="notice notice-error" role="alert">
          <p>{notice.summary}</p>
          {notice.details && <details>
            <summary>Show details</summary>
            <pre>{notice.details}</pre>
            <div className="notice-actions">
              <button type="button" className="link" onClick={() => void copyDetails(`${appName} ${version ?? "unknown version"}\n${notice.details ?? ""}`)}>{copied ? "Copied" : "Copy details"}</button>
              {diagnosticsPath && <span className="hint">Full log: {diagnosticsPath}</span>}
            </div>
          </details>}
        </section>
      : <p className="notice" role="status">{notice.summary}</p>)}
    <footer><span><button className="link" disabled={busy} onClick={() => void checkUpdates()}>Check for updates</button>{version && <span className="version"> · v{version}</span>}</span><span>Closing this window keeps the tray app running.</span></footer>
  </main>;
}
