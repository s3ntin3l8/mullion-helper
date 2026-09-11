import { useCallback, useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { enable, disable, isEnabled } from "@tauri-apps/plugin-autostart";
import { api } from "./api";
import type { BridgeStatus, Settings } from "./types";

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
  const [notice, setNotice] = useState<string | null>(null);
  const [updateVersion, setUpdateVersion] = useState<string | null>(null);
  const refresh = useCallback(async () => {
    const [nextStatus, nextSettings] = await Promise.all([api.status(), api.settings()]);
    if (api.isDesktop) nextSettings.launch_at_login = await isEnabled();
    setStatus(nextStatus); setSettings(nextSettings);
  }, []);

  useEffect(() => {
    void refresh().catch((error: unknown) => setNotice(String(error)));
    if (!api.isDesktop) return;
    let unlisten = () => {};
    void listen<BridgeStatus>("bridge-status", (event) => setStatus(event.payload)).then((fn) => { unlisten = fn; });
    return () => unlisten();
  }, [refresh]);

  useEffect(() => {
    if (!api.isDesktop) return;
    const check = () => void api.checkForUpdates().then((result) => {
      if (result.available) setUpdateVersion(result.version);
    }).catch(() => {});
    const initial = window.setTimeout(check, 3000);
    const daily = window.setInterval(check, 24 * 60 * 60 * 1000);
    return () => { window.clearTimeout(initial); window.clearInterval(daily); };
  }, []);

  async function act(operation: () => Promise<BridgeStatus>) {
    setBusy(true); setNotice(null);
    try { setStatus(await operation()); } catch (error) { setNotice(String(error)); }
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
  async function save() {
    if (!settings) return;
    setBusy(true); setNotice(null);
    try {
      if (api.isDesktop) {
        if (settings.launch_at_login) await enable(); else await disable();
      }
      setSettings(await api.saveSettings(settings));
      setNotice("Settings saved. The bridge was restarted with the new configuration.");
    } catch (error) { setNotice(String(error)); } finally { setBusy(false); }
  }
  async function checkUpdates() {
    setBusy(true);
    try {
      const result = await api.checkForUpdates();
      setUpdateVersion(result.available ? result.version : null);
      setNotice(result.available ? `Version ${result.version} is available.` : "Mullion Helper is up to date.");
    } catch (error) { setNotice(String(error)); } finally { setBusy(false); }
  }

  const connected = status?.state === "connected";
  const running = status && !["paused", "unpaired", "needs_pairing", "error"].includes(status.state);
  const needsPairing = status && ["unpaired", "needs_pairing"].includes(status.state);
  return <main className="shell">
    <header><BrandMark /><div><h1>Mullion Helper</h1><p>Your local SSH-agent bridge</p></div></header>
    <section className="status-card" aria-live="polite">
      <span className={`orb ${status?.state ?? "starting"}`} />
      <div className="status-copy"><strong>{status ? labels[status.state] : "Loading…"}</strong><span>{status?.detail ?? (connected ? "Your SSH agent is available to Mullion sessions." : "The tray icon keeps the bridge available in the background.")}</span>{status?.base_url && <small>{status.base_url}</small>}</div>
      {!needsPairing && (running ? <button className="secondary" disabled={busy} onClick={() => void act(api.pause)}>Pause</button> : <button disabled={busy} onClick={() => void act(api.start)}>Start</button>)}
    </section>
    {needsPairing && <section className="panel onboarding"><span className="eyebrow">First-time setup</span><h2>Connect this computer</h2><p>In Mullion, open Settings → Hosts → SSH agent bridges, create a pairing code, then paste the payload below.</p><label>Pairing payload<textarea value={payload} onChange={(event) => setPayload(event.target.value)} placeholder="Paste pairing payload" rows={3} /></label><button disabled={busy || !payload.trim()} onClick={() => void pair()}>{busy ? "Pairing…" : "Pair and start"}</button></section>}
    {!needsPairing && settings && <section className="panel"><span className="eyebrow">Configuration</span><h2>Settings</h2><label>SSH agent socket<input value={settings.ssh_auth_sock} onChange={(event) => setSettings({ ...settings, ssh_auth_sock: event.target.value })} placeholder="Auto-detect" /></label><p className="hint">Leave blank to detect SSH_AUTH_SOCK, 1Password, or the Windows OpenSSH-compatible pipe.</p><label className="toggle"><input type="checkbox" checked={settings.launch_at_login} onChange={(event) => setSettings({ ...settings, launch_at_login: event.target.checked })} /><span>Launch at login</span></label><label className="toggle"><input type="checkbox" checked={settings.insecure} onChange={(event) => setSettings({ ...settings, insecure: event.target.checked })} /><span>Allow self-signed TLS certificates</span></label><button disabled={busy} onClick={() => void save()}>Save settings</button></section>}
    {updateVersion && <section className="update"><div><strong>Mullion Helper {updateVersion} is available</strong><span>The app will restart after installing.</span></div><button disabled={busy} onClick={() => { setBusy(true); void api.installUpdate().catch((error) => { setNotice(String(error)); setBusy(false); }); }}>Install update</button></section>}
    {notice && <p className="notice" role="status">{notice}</p>}
    <footer><button className="link" disabled={busy} onClick={() => void checkUpdates()}>Check for updates</button><span>Closing this window keeps the tray app running.</span></footer>
  </main>;
}
