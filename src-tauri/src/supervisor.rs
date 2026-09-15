use crate::{headless_process, migration::MigrationState, tray_status::TrayStatus};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, Runtime, Wry};

const WINDOWS_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
const HEALTHY_CONNECTION_RESET_AFTER: Duration = Duration::from_secs(30);
// How long the bridge can stay unhealthy (repeated connect_failed/
// disconnected events, no intervening "connected") before the supervisor
// tears the worker down and lets its own restart ladder respawn it fresh.
// The worker's own internal reconnect ladder already retries forever on its
// own (RECONNECT_DELAYS_MS in helper.mjs, topping out at 30s), which is
// enough for an ordinary blip -- this exists for the failure mode a plain
// retry ladder can't fix on its own: a live process stuck talking to a
// server or network path that's transiently broken in a way a fresh
// process (fresh DNS resolution, fresh TLS session, fresh TCP connection
// pool) recovers from but the SAME process retrying the SAME broken state
// never does. Long enough that a genuine transient (server restart, brief
// route flap) resolves well before this fires; short enough that a repeat
// of the reported 4-hour outage self-heals in minutes instead.
const SUSTAINED_FAILURE_RESTART_AFTER: Duration = Duration::from_secs(180);

// Wire format for the probe this module speaks directly to a candidate SSH
// agent -- entirely separate from the worker's own mux (see the doc comment
// on `probe_agent` for why that separation is deliberate). Mirrors
// src/worker/ssh-agent-protocol-v1.json: a 4-byte big-endian length prefix,
// SSH_AGENTC_REQUEST_IDENTITIES = 11 (the only outbound type this probe ever
// sends), SSH_AGENT_IDENTITIES_ANSWER = 12, and the same 256KiB frame cap.
const REQUEST_IDENTITIES_FRAME: [u8; 5] = [0, 0, 0, 1, 11];
const SSH_AGENT_IDENTITIES_ANSWER: u8 = 12;
const MAX_AGENT_PROBE_FRAME_BYTES: usize = 262_144;
// Local IPC (a Unix socket or a Windows named pipe already on disk) either
// answers in single-digit milliseconds or is stuck -- this only needs to be
// long enough to not misclassify a slow-but-alive agent (e.g. 1Password
// waking up) as unreachable, not long enough to matter to a human waiting
// for the app to start.
const AGENT_PROBE_TIMEOUT: Duration = Duration::from_millis(750);
// Bounds the leak from a wedged Windows named pipe -- see the doc comment
// on `probe_agent`'s Windows branch. There are at most a handful of
// candidates per resolution (`agent_candidate_paths`), so this is generous
// for legitimate concurrent probing while still capping the worst case.
#[cfg(windows)]
const MAX_CONCURRENT_WINDOWS_AGENT_PROBES: usize = 8;
#[cfg(windows)]
static PENDING_WINDOWS_AGENT_PROBES: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
// How long a path that just timed out is skipped before being probed again.
// Long enough that a genuinely wedged pipe isn't re-spawned on every
// resolution (every worker restart); short enough that a since-fixed pipe
// (service restarted, pipe re-created) recovers well within a support
// conversation rather than needing an app restart.
#[cfg(windows)]
const WINDOWS_PROBE_COOLDOWN: Duration = Duration::from_secs(5 * 60);
#[cfg(windows)]
static WINDOWS_PROBE_COOLDOWNS: std::sync::LazyLock<
    Mutex<std::collections::HashMap<String, Instant>>,
> = std::sync::LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

/// Releases a `PENDING_WINDOWS_AGENT_PROBES` slot exactly once no matter
/// which side -- the caller giving up in `probe_agent`, or the probe thread
/// itself finishing late -- gets there first. `AtomicBool::compare_exchange`
/// makes the race safe: only the side that flips `false` -> `true` actually
/// decrements, so a slot already reclaimed by a caller timeout is never
/// double-released when the thread eventually completes too.
#[cfg(windows)]
fn release_windows_probe_slot(released: &AtomicBool) {
    if released
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
    {
        PENDING_WINDOWS_AGENT_PROBES.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BridgeState {
    Unpaired,
    Starting,
    Connected,
    Reconnecting,
    Paused,
    NeedsPairing,
    AgentUnavailable,
    Error,
}

#[derive(Clone, Debug, Serialize)]
pub struct BridgeStatus {
    pub state: BridgeState,
    pub base_url: Option<String>,
    pub bridge_id: Option<String>,
    pub detail: Option<String>,
    pub retry_in_ms: Option<u64>,
    pub updated_at: String,
    // Identity count from the most recent agent resolution, regardless of
    // which BridgeState it's attached to -- see `set_status`, which stamps
    // this onto every outgoing status from `Inner::agent_identities` rather
    // than requiring each call site to know about it. `Some(0)` is the
    // "connected but useless" case this exists to surface; `None` means no
    // agent has been resolved yet (paused/unpaired) or the count is unknown.
    pub agent_identities: Option<u32>,
    // How many "connect_failed" events (never even reaching a connection,
    // as opposed to "disconnected" -- see handle_event) have fired since
    // the last successful "connected". Stamped by `set_status` from
    // `Inner::consecutive_connect_failures` the same way `agent_identities`
    // is, so it's visible on every status regardless of which event
    // constructed it. The frontend uses this to escalate its presentation
    // of a stuck Reconnecting state; it never drives a state change here
    // (see PR discussion on not adding a new BridgeState for this).
    pub consecutive_connect_failures: u32,
}

impl BridgeStatus {
    fn new(state: BridgeState, detail: Option<String>) -> Self {
        Self {
            state,
            base_url: None,
            bridge_id: None,
            detail,
            retry_in_ms: None,
            updated_at: Utc::now().to_rfc3339(),
            agent_identities: None,
            consecutive_connect_failures: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Settings {
    pub ssh_auth_sock: String,
    pub insecure: bool,
    #[serde(default)]
    pub launch_at_login: bool,
}

/// A candidate SSH agent endpoint offered to the user in Settings, and the
/// same data `resolve_agent` uses internally to choose one automatically.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct AgentCandidate {
    pub path: String,
    pub label: &'static str,
    pub reachable: bool,
    pub identities: Option<u32>,
}

#[derive(Deserialize)]
struct Inspection {
    paired: bool,
    base_url: Option<String>,
    bridge_id: Option<String>,
}

struct Inner<R: Runtime> {
    app: AppHandle<R>,
    worker: PathBuf,
    data_dir: PathBuf,
    desired: AtomicBool,
    shutdown: AtomicBool,
    child: Mutex<Option<Child>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    blocking_shutdown: Mutex<()>,
    status: Mutex<BridgeStatus>,
    settings: Mutex<Settings>,
    connected_at: Mutex<Option<Instant>>,
    agent_identities: Mutex<Option<u32>>,
    consecutive_connect_failures: AtomicU32,
    // Wall-clock start of the current unhealthy streak (first
    // connect_failed/disconnected since the last connected), or None while
    // healthy/idle. Drives the sustained-failure restart in handle_event --
    // deliberately separate from `consecutive_connect_failures`, which is
    // attempt-counted and purely for display (see SUSTAINED_FAILURE_
    // RESTART_AFTER's doc comment on why the restart trigger must be
    // time-based instead).
    unhealthy_since: Mutex<Option<Instant>>,
}

pub struct Supervisor<R: Runtime = Wry>(Arc<Inner<R>>);

impl<R: Runtime> Clone for Supervisor<R> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<R: Runtime> Supervisor<R> {
    pub fn new(app: AppHandle<R>, data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(data_dir.join("worker")).map_err(|error| error.to_string())?;
        let settings = read_settings(&data_dir);
        Ok(Self(Arc::new(Inner {
            app,
            worker: worker_path()?,
            data_dir,
            desired: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            child: Mutex::new(None),
            thread: Mutex::new(None),
            blocking_shutdown: Mutex::new(()),
            status: Mutex::new(BridgeStatus::new(BridgeState::Unpaired, None)),
            settings: Mutex::new(settings),
            connected_at: Mutex::new(None),
            agent_identities: Mutex::new(None),
            consecutive_connect_failures: AtomicU32::new(0),
            unhealthy_since: Mutex::new(None),
        })))
    }

    pub fn launch(&self) {
        let _shutdown = self
            .0
            .blocking_shutdown
            .lock()
            .expect("shutdown mutex poisoned");
        if self.0.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let supervisor = self.clone();
        let handle = thread::spawn(move || supervisor.run_loop());
        *self.0.thread.lock().expect("thread mutex poisoned") = Some(handle);
    }

    pub fn status(&self) -> BridgeStatus {
        self.0.status.lock().expect("status mutex poisoned").clone()
    }

    pub fn settings(&self) -> Settings {
        self.0
            .settings
            .lock()
            .expect("settings mutex poisoned")
            .clone()
    }

    pub fn save_settings(&self, settings: Settings) -> Result<Settings, String> {
        write_json_atomic(&self.0.data_dir.join("settings.json"), &settings)?;
        *self
            .0
            .settings
            .lock()
            .map_err(|_| "settings mutex poisoned")? = settings.clone();
        if self.0.desired.load(Ordering::SeqCst) {
            self.stop_child();
            self.reset_failure_tracking();
        }
        Ok(settings)
    }

    pub fn start(&self) -> BridgeStatus {
        self.0.desired.store(true, Ordering::SeqCst);
        self.set_status(BridgeStatus::new(BridgeState::Starting, None));
        self.status()
    }

    pub fn pause(&self) -> BridgeStatus {
        self.0.desired.store(false, Ordering::SeqCst);
        self.stop_child();
        // No agent is in active use while paused -- don't let set_status go
        // on stamping a stale identity count onto a state where it no
        // longer applies.
        *self
            .0
            .agent_identities
            .lock()
            .expect("agent identities mutex poisoned") = None;
        self.reset_failure_tracking();
        self.set_status(BridgeStatus::new(BridgeState::Paused, None));
        self.status()
    }

    pub fn shutdown(&self) {
        self.0.shutdown.store(true, Ordering::SeqCst);
        self.0.desired.store(false, Ordering::SeqCst);
        self.stop_child();
    }

    /// Stops every process owned by the supervisor and waits for the loop to
    /// finish. The updater uses this before NSIS replaces the bundled worker.
    #[cfg(any(windows, test))]
    pub fn shutdown_for_update(&self) {
        let _shutdown = self
            .0
            .blocking_shutdown
            .lock()
            .expect("shutdown mutex poisoned");
        self.shutdown();
        join_thread(&self.0.thread);
    }

    pub fn inspect(&self) -> Result<bool, String> {
        Ok(self.inspect_detail()?.paired)
    }

    pub fn pair(&self, payload: &str) -> Result<BridgeStatus, String> {
        if payload.trim().is_empty() || payload.len() > 8192 {
            return Err("The pairing payload is empty or too large.".into());
        }
        self.0.desired.store(false, Ordering::SeqCst);
        self.stop_child();
        let settings = self.settings();
        let mut command = self.worker_command();
        command.args(["pair", "--payload-stdin", "--json"]);
        if settings.insecure {
            command.arg("--insecure");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("Could not start the bundled bridge worker: {error}"))?;
        child
            .stdin
            .take()
            .ok_or("Could not open worker input")?
            .write_all(payload.trim().as_bytes())
            .map_err(|error| error.to_string())?;
        let output = child
            .wait_with_output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(clean_worker_error(&output.stderr));
        }
        let reply: Value = serde_json::from_slice(&output.stdout)
            .map_err(|_| "The bridge worker returned an invalid pairing response".to_string())?;
        let mut status = BridgeStatus::new(BridgeState::Starting, None);
        status.base_url = reply
            .get("base_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        status.bridge_id = reply
            .get("bridge_id")
            .and_then(Value::as_str)
            .map(str::to_owned);
        self.set_status(status);
        self.0.desired.store(true, Ordering::SeqCst);
        Ok(self.status())
    }

    /// Forgets this computer's pairing so it can be re-paired against a
    /// different (or the same) primary. Stops the child and waits for it to
    /// exit *before* deleting the credential file — the live worker rotates
    /// the session on its own schedule and would otherwise be able to
    /// rewrite the file out from under this deletion, same ordering
    /// `pair()` above already relies on via `stop_child()`. Deleting the
    /// file is sufficient on its own: `run_loop`'s `inspect` check picks up
    /// `paired: false` on its next iteration and moves to `Unpaired`, but
    /// setting the status here too means the UI updates immediately rather
    /// than waiting for that poll. `ssh-agent-bridge.json` is the worker's
    /// *only* persisted state — `saveCredential` in helper.mjs is the sole
    /// write site, shared by `pair` and session renewal — so deleting it is
    /// a complete, not partial, unpair.
    pub fn unpair(&self) -> Result<BridgeStatus, String> {
        self.0.desired.store(false, Ordering::SeqCst);
        self.stop_child();
        *self
            .0
            .agent_identities
            .lock()
            .expect("agent identities mutex poisoned") = None;
        self.reset_failure_tracking();
        let credential_path = self.0.data_dir.join("worker/ssh-agent-bridge.json");
        if let Err(error) = fs::remove_file(&credential_path) {
            // Already unpaired (or never paired): treat as success rather
            // than surfacing an error for a state the caller already wants.
            if error.kind() != std::io::ErrorKind::NotFound {
                // Leaving the previous status (e.g. Connected/Reconnecting)
                // in place here would be actively wrong: desired is already
                // false and the child is already stopped, so nothing is
                // running regardless of whether the delete succeeded. An
                // error here means the credential might still be on disk,
                // so report Error rather than claiming Unpaired.
                let message = error.to_string();
                self.set_status(BridgeStatus::new(BridgeState::Error, Some(message.clone())));
                return Err(message);
            }
        }
        // A never-completed legacy-tool migration (imported at startup but
        // never reached a successful `connected` event -- e.g. a
        // paired-but-unreachable install, the exact shape of the outage
        // this command exists to recover from) leaves the marker file
        // unwritten and the original legacy credential untouched on disk.
        // Without finishing it here, the next launch's
        // `import_legacy_credential` would silently re-copy that same
        // credential right back into the file just deleted above --
        // undoing this unpair on restart. Treat "the user unpaired" as
        // equivalent to "migration complete": finish it now, the same way
        // a successful connect already does in `handle_event`.
        if let Some(state) = self.0.app.try_state::<MigrationState>() {
            if let Some(migration) = state.0.lock().expect("migration mutex poisoned").take() {
                if let Err(error) = migration.commit() {
                    // commit() collapses two different failure modes into
                    // one Result: disabling the legacy service (which runs
                    // FIRST and short-circuits the rest via `?`) failing,
                    // and the marker write itself failing. Don't assume
                    // which one happened — check. If the marker genuinely
                    // wasn't written, the exact resurrection risk this
                    // whole block exists to close is still open on the
                    // next launch, which is worth surfacing loudly (Error)
                    // rather than as an easy-to-miss detail string. Only
                    // treat it as a benign cleanup hiccup if the marker
                    // exists despite the reported error.
                    let marker_written = self.0.data_dir.join("legacy-migration.json").exists();
                    self.set_status(BridgeStatus::new(
                        if marker_written {
                            BridgeState::Unpaired
                        } else {
                            BridgeState::Error
                        },
                        Some(error.clone()),
                    ));
                    return if marker_written {
                        Ok(self.status())
                    } else {
                        Err(error)
                    };
                }
            }
        }
        self.set_status(BridgeStatus::new(BridgeState::Unpaired, None));
        Ok(self.status())
    }

    fn run_loop(&self) {
        let mut attempt = 0usize;
        while !self.0.shutdown.load(Ordering::SeqCst) {
            if !self.0.desired.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(200));
                continue;
            }
            let inspection = match self.inspect_detail() {
                Ok(value) => value,
                Err(error) => {
                    self.set_status(BridgeStatus::new(BridgeState::Error, Some(error)));
                    self.interruptible_sleep(Duration::from_secs(2));
                    continue;
                }
            };
            if !inspection.paired {
                self.0.desired.store(false, Ordering::SeqCst);
                self.set_status(BridgeStatus::new(BridgeState::Unpaired, None));
                continue;
            }
            let resolution = match resolve_agent(&self.settings()) {
                Some(value) => value,
                None => {
                    // No candidate was even reachable -- a prior successful
                    // resolution's identity count must not leak onto this
                    // status (see the `pause`/`unpair` precedent above and
                    // `agent_identities`'s doc comment: `None` means "no
                    // agent resolved", which is exactly this branch).
                    *self
                        .0
                        .agent_identities
                        .lock()
                        .expect("agent identities mutex poisoned") = None;
                    self.set_status(BridgeStatus::new(BridgeState::AgentUnavailable, Some("No SSH agent socket was found. Open your SSH agent or configure its socket in Settings.".into())));
                    self.interruptible_sleep(Duration::from_secs(3));
                    continue;
                }
            };
            log::info!(
                "resolved SSH agent socket {} with {} identities",
                resolution.path,
                resolution
                    .identities
                    .map(|count| count.to_string())
                    .unwrap_or_else(|| "unknown".into())
            );
            *self
                .0
                .agent_identities
                .lock()
                .expect("agent identities mutex poisoned") = resolution.identities;
            let socket = resolution.path;
            let mut status = BridgeStatus::new(BridgeState::Starting, None);
            status.base_url = inspection.base_url;
            status.bridge_id = inspection.bridge_id;
            self.set_status(status);
            *self
                .0
                .connected_at
                .lock()
                .expect("connection mutex poisoned") = None;
            let settings = self.settings();
            let mut command = self.worker_command();
            command
                .args(["run", "--json-events", "--ssh-auth-sock"])
                .arg(socket);
            if settings.insecure {
                command.arg("--insecure");
            }
            let spawn = command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();
            let mut child = match spawn {
                Ok(value) => value,
                Err(error) => {
                    self.set_status(BridgeStatus::new(
                        BridgeState::Error,
                        Some(format!(
                            "Could not start the bundled bridge worker: {error}"
                        )),
                    ));
                    self.interruptible_sleep(Duration::from_secs(2));
                    continue;
                }
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            *self.0.child.lock().expect("child mutex poisoned") = Some(child);
            if let Some(stdout) = stdout {
                let supervisor = self.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                        supervisor.handle_event(&line);
                    }
                });
            }
            if let Some(stderr) = stderr {
                let supervisor = self.clone();
                thread::spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        supervisor.handle_stderr(&line);
                    }
                });
            }
            loop {
                if !self.0.desired.load(Ordering::SeqCst) || self.0.shutdown.load(Ordering::SeqCst)
                {
                    self.stop_child();
                    break;
                }
                let exited = self
                    .0
                    .child
                    .lock()
                    .expect("child mutex poisoned")
                    .as_mut()
                    .is_none_or(|child| child.try_wait().ok().flatten().is_some());
                if exited {
                    break;
                }
                thread::sleep(Duration::from_millis(200));
            }
            self.reap_child();
            if self.0.desired.load(Ordering::SeqCst) && !self.0.shutdown.load(Ordering::SeqCst) {
                let connected_at = self
                    .0
                    .connected_at
                    .lock()
                    .expect("connection mutex poisoned")
                    .take();
                if should_reset_backoff(connected_at) {
                    attempt = 0;
                }
                let delays = [1, 2, 5, 10, 30];
                let delay = delays[attempt.min(delays.len() - 1)];
                attempt += 1;
                let mut status = self.status();
                status.state = BridgeState::Reconnecting;
                status.retry_in_ms = Some(delay * 1000);
                status.updated_at = Utc::now().to_rfc3339();
                self.set_status(status);
                self.interruptible_sleep(Duration::from_secs(delay));
            }
        }
    }

    fn handle_event(&self, line: &str) {
        // A supervisor-initiated stop (pause/unpair/re-pair) sets `desired =
        // false` *before* killing the child, but the child's stdout reader
        // thread (spawned in run_loop, never joined by stop_child()) can
        // still be draining lines the worker wrote just before it was
        // killed. Without this gate, a stray late "connected" /
        // "connect_failed" / "dead_credential" line can overwrite the
        // terminal status (Paused/Unpaired) the caller just set — e.g. the
        // user clicks "Unpair" and the UI flips back to "Reconnecting" a
        // moment later even though the credential is already gone and
        // nothing is running. Once `desired` is false the supervisor has
        // already decided the bridge shouldn't be running; nothing the
        // worker says from here counts. (This narrows the race to the
        // interval between this check and the caller's own set_status
        // call, rather than eliminating it outright — stop_child() kills
        // the process itself, so no *new* events can be produced after
        // that point, only already-buffered ones drained late.)
        if !self.0.desired.load(Ordering::SeqCst) {
            return;
        }
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        match event.get("type").and_then(Value::as_str) {
            Some("connected") => {
                *self
                    .0
                    .connected_at
                    .lock()
                    .expect("connection mutex poisoned") = Some(Instant::now());
                self.reset_failure_tracking();
                let mut status = BridgeStatus::new(BridgeState::Connected, None);
                status.base_url = event
                    .get("base_url")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                status.bridge_id = event
                    .get("bridge_id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.set_status(status);
                if let Some(state) = self.0.app.try_state::<MigrationState>() {
                    if let Some(migration) =
                        state.0.lock().expect("migration mutex poisoned").take()
                    {
                        if let Err(error) = migration.commit() {
                            // Unlike unpair()'s commit()-failure handling
                            // below, this is always Error, unconditionally
                            // — deliberately not mirroring unpair()'s
                            // "check whether the marker actually landed"
                            // nuance. Here the user's intent (be connected,
                            // with a clean, committed migration) failed
                            // outright: the bridge is stopped in response.
                            // In unpair()'s case the user's intent
                            // (be unpaired) already succeeded regardless of
                            // this failure — the only question is whether a
                            // *future* restart is still safe, which is what
                            // that check answers.
                            self.0.desired.store(false, Ordering::SeqCst);
                            self.stop_child();
                            self.set_status(BridgeStatus::new(BridgeState::Error, Some(error)));
                        }
                    }
                }
            }
            Some(event_type @ ("disconnected" | "connect_failed")) => {
                let message = event.get("message").and_then(Value::as_str);
                // Same "log before it can be dropped" discipline as
                // handle_stderr: `detail` here is neither truncated nor
                // state-gated today, but it IS ephemeral (overwritten by the
                // next status update) and never persisted anywhere else.
                if let Some(message) = message {
                    log::warn!("worker reported {event_type}: {message}");
                }
                // Attempt-counted and display-only -- only a connect_failed
                // (never even reached a connection) counts, matching the
                // reported bug's own signature (hundreds of back-to-back
                // "connection error"s). A "disconnected" means a connection
                // DID succeed at some point in this streak, which is a
                // materially better signal than never connecting at all,
                // even though both still leave the bridge un-connected now.
                if event_type == "connect_failed" {
                    self.0
                        .consecutive_connect_failures
                        .fetch_add(1, Ordering::SeqCst);
                }
                // Time-based, not attempt-based -- see SUSTAINED_FAILURE_
                // RESTART_AFTER's doc comment for why. Both event types
                // count toward the streak: either one means "not currently
                // connected," which is the actual condition the restart
                // exists to break out of.
                let now = Instant::now();
                let restart_worker = {
                    let mut unhealthy_since = self
                        .0
                        .unhealthy_since
                        .lock()
                        .expect("unhealthy-streak mutex poisoned");
                    let started_at = *unhealthy_since.get_or_insert(now);
                    if should_restart_for_sustained_failure(started_at, now) {
                        // Re-arm immediately rather than after the restart
                        // completes: the freshly spawned child gets a full
                        // new streak budget, not zero, and this can't race
                        // the child's own exit (this is the same thread that
                        // is about to call stop_child() below). Same
                        // reasoning for the display counter -- without this,
                        // the very first status after a just-triggered
                        // restart would still show the pre-restart count
                        // (already >= the frontend's escalation threshold),
                        // reading as "still stuck" about the restart that
                        // was just supposed to fix it.
                        *unhealthy_since = None;
                        self.0
                            .consecutive_connect_failures
                            .store(0, Ordering::SeqCst);
                        true
                    } else {
                        false
                    }
                };
                let mut status = self.status();
                status.state = BridgeState::Reconnecting;
                status.detail = message.map(str::to_owned);
                status.updated_at = Utc::now().to_rfc3339();
                self.set_status(status);
                if restart_worker {
                    log::warn!(
                        "bridge unhealthy for over {}s with no successful connection -- restarting the worker",
                        SUSTAINED_FAILURE_RESTART_AFTER.as_secs()
                    );
                    // Not stop() / pause(): `desired` stays true, so
                    // run_loop's own exited-child detection (already
                    // polling `self.0.child`) reaps this and respawns
                    // through its normal path, backoff ladder and all --
                    // reusing the exact teardown pause()/unpair() already
                    // use rather than a second kill path with its own
                    // bookkeeping gaps.
                    self.stop_child();
                }
            }
            Some(event_type @ ("dead_credential" | "renewal_rejected")) => {
                // The worker's own `message` is replaced with fixed prose below
                // (it's written for a log, not a user), but it's still logged
                // here so a report of "pairing required again" has something
                // to look at instead of nothing.
                if let Some(message) = event.get("message").and_then(Value::as_str) {
                    log::warn!("worker reported {event_type}: {message}");
                }
                self.0.desired.store(false, Ordering::SeqCst);
                self.set_status(BridgeStatus::new(
                    BridgeState::NeedsPairing,
                    Some(
                        "The saved bridge session is no longer valid. Pair this computer again."
                            .into(),
                    ),
                ));
            }
            _ => {}
        }
    }

    fn handle_stderr(&self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        // Logged in full and unconditionally, BEFORE the state gate below —
        // the UI-facing `detail` field is capped at 500 chars and dropped
        // entirely outside Starting/Connected/Reconnecting, which is exactly
        // when a user is most likely staring at the tray wondering why. The
        // log is the only place this text survives in those states.
        log::warn!("worker: {trimmed}");
        let detail: String = trimmed.chars().take(500).collect();
        let mut status = self.0.status.lock().expect("status mutex poisoned");
        if matches!(
            status.state,
            BridgeState::Starting | BridgeState::Connected | BridgeState::Reconnecting
        ) {
            status.detail = Some(detail);
            status.updated_at = Utc::now().to_rfc3339();
            let snapshot = status.clone();
            drop(status);
            let _ = self.0.app.emit("bridge-status", snapshot);
        }
    }

    fn inspect_detail(&self) -> Result<Inspection, String> {
        let output = self
            .worker_command()
            .args(["inspect", "--json"])
            .output()
            .map_err(|error| format!("Could not inspect the bundled bridge worker: {error}"))?;
        if !output.status.success() {
            return Err(clean_worker_error(&output.stderr));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|_| "The bundled bridge worker returned invalid status".into())
    }

    fn worker_command(&self) -> Command {
        let mut command = headless_process::command(&self.0.worker);
        command.env("MULLION_HELPER_STATE_DIR", self.0.data_dir.join("worker"));
        command
    }

    fn stop_child(&self) {
        stop_child(&self.0.child);
    }

    fn reap_child(&self) {
        reap_child(&self.0.child);
    }

    fn set_status(&self, mut status: BridgeStatus) {
        status.agent_identities = *self
            .0
            .agent_identities
            .lock()
            .expect("agent identities mutex poisoned");
        status.consecutive_connect_failures =
            self.0.consecutive_connect_failures.load(Ordering::SeqCst);
        *self.0.status.lock().expect("status mutex poisoned") = status.clone();
        if let Some(tray_status) = self.0.app.try_state::<TrayStatus<R>>() {
            tray_status.update(&status);
        }
        let _ = self.0.app.emit("bridge-status", status);
    }

    // A paused/unpaired/reconfigured bridge isn't mid-connection-attempt --
    // resetting both here means the next time it actually tries to
    // reconnect, it starts a fresh streak rather than inheriting an elapsed
    // duration from before the pause/settings change, which could otherwise
    // trip the sustained-failure restart on the very first attempt after
    // resuming.
    fn reset_failure_tracking(&self) {
        self.0
            .consecutive_connect_failures
            .store(0, Ordering::SeqCst);
        *self
            .0
            .unhealthy_since
            .lock()
            .expect("unhealthy-streak mutex poisoned") = None;
    }

    fn interruptible_sleep(&self, duration: Duration) {
        let mut remaining = duration;
        while remaining > Duration::ZERO
            && self.0.desired.load(Ordering::SeqCst)
            && !self.0.shutdown.load(Ordering::SeqCst)
        {
            let step = remaining.min(Duration::from_millis(200));
            thread::sleep(step);
            remaining -= step;
        }
    }
}

fn stop_child(child: &Mutex<Option<Child>>) -> Option<ExitStatus> {
    let mut child = child.lock().expect("child mutex poisoned").take()?;
    let _ = child.kill();
    child.wait().ok()
}

fn reap_child(child: &Mutex<Option<Child>>) -> Option<ExitStatus> {
    child
        .lock()
        .expect("child mutex poisoned")
        .take()?
        .wait()
        .ok()
}

#[cfg(any(windows, test))]
fn join_thread(thread: &Mutex<Option<JoinHandle<()>>>) {
    if let Some(handle) = thread.lock().expect("thread mutex poisoned").take() {
        let _ = handle.join();
    }
}

fn read_settings(data_dir: &Path) -> Settings {
    fs::read(data_dir.join("settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(path).map_err(|error| error.to_string())?;
    }
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

/// True for Apple's on-demand per-session ssh-agent socket, created for
/// every GUI login on macOS. It exists for the whole session and reports
/// zero identities unless the user explicitly ran `ssh-add
/// --apple-use-keychain` — so `resolve_agent_socket` must not trust it over
/// a real agent like 1Password just because `SSH_AUTH_SOCK` happens to
/// point at it (a GUI app, including this one under its LaunchAgent,
/// inherits it from launchd regardless of whether 1Password is also
/// running). Reported symptom: the bridge showed `connected` while
/// forwarding zero keys.
///
/// The socket always lives at `<some launchd-owned dir>/com.apple.launchd.
/// <opaque-id>/Listeners` — matched here by the last two path segments,
/// NOT a hardcoded directory prefix. An earlier version of this function
/// checked for a literal `/private/tmp/` prefix on the theory that this is
/// always where the socket lives; `launchctl getenv SSH_AUTH_SOCK` on a
/// real reporting machine came back `/var/run/com.apple.launchd.<id>/
/// Listeners` instead, which that prefix check silently failed to match —
/// so the very bug this function exists to fix was still live after that
/// version merged. Match on shape, not location.
///
/// The path shape is unambiguous on any platform (no non-Apple system ever
/// produces a `com.apple.launchd.*` path segment), so this is checked
/// unconditionally rather than gated on `cfg!(target_os = "macos")` —
/// which also means the regression tests below exercise the real code path
/// on Linux CI instead of silently no-op'ing.
///
/// A trailing slash is trimmed before matching. launchd doesn't hand out
/// `SSH_AUTH_SOCK` with one, so this isn't reachable today, but the
/// direction this function fails open in matters: a false negative here
/// means trusting the empty launchd agent over 1Password again — the exact
/// bug class this function exists to prevent — so it's worth the one line
/// even for an input shape nothing currently produces.
fn is_macos_launchd_socket(path: &str) -> bool {
    let path = path.trim_end_matches('/');
    path.ends_with("/Listeners")
        && path
            .rsplit('/')
            .nth(1) // the segment before the "Listeners" leaf, i.e. the parent dir name
            .is_some_and(|segment| segment.starts_with("com.apple.launchd."))
}

/// Builds the ordered list of auto-detect candidates -- paths and their
/// display labels only, no reachability or identity information yet, that's
/// `probe_candidates`' job. Order is the precedence `choose_best` falls back
/// through when nothing reports identities: a non-launchd `SSH_AUTH_SOCK`,
/// then the two 1Password paths, then the launchd socket last (it isn't a
/// real fallback candidate at all before this last position -- see
/// `is_macos_launchd_socket`'s doc comment on why it must be deferred).
///
/// On Windows this is just `SSH_AUTH_SOCK` (if set) then the well-known
/// OpenSSH pipe -- no existence check on the env var, matching the
/// unconditional trust this branch already gave it before probing existed;
/// probing now does the reachability work that check used to approximate.
fn agent_candidate_paths() -> Vec<(String, &'static str)> {
    let mut candidates = Vec::new();
    let env_sock = env::var("SSH_AUTH_SOCK").ok();
    if cfg!(windows) {
        if let Some(sock) = env_sock {
            candidates.push((sock, "SSH_AUTH_SOCK"));
        }
        candidates.push((WINDOWS_AGENT_PIPE.to_owned(), "OpenSSH agent pipe"));
        return candidates;
    }
    let env_is_launchd = env_sock.as_deref().is_some_and(is_macos_launchd_socket);
    if let Some(sock) = env_sock.clone() {
        if !env_is_launchd {
            candidates.push((sock, "SSH_AUTH_SOCK"));
        }
    }
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.push((
            home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock")
                .to_string_lossy()
                .into_owned(),
            "1Password",
        ));
        candidates.push((
            home.join(".1password/agent.sock")
                .to_string_lossy()
                .into_owned(),
            "1Password (legacy path)",
        ));
    }
    if let Some(sock) = env_sock {
        if env_is_launchd {
            candidates.push((sock, "macOS login agent"));
        }
    }
    candidates
}

/// Probes each candidate in order and returns the full picture -- this is
/// what both `resolve_agent` and the `list_agent_sockets` command (the
/// Settings dropdown) consume, so the dropdown always shows exactly what
/// auto-detect itself just saw. Sequential, not parallel: this only runs
/// once per resolution (agent startup, a settings save, or a sustained
/// failure restart), not per connection attempt, so a worst case of a few
/// candidates times `AGENT_PROBE_TIMEOUT` is a startup-latency cost, not a
/// per-second one.
fn probe_candidates(candidates: Vec<(String, &'static str)>) -> Vec<AgentCandidate> {
    candidates
        .into_iter()
        .map(|(path, label)| {
            let (reachable, identities) = probe_agent(&path);
            AgentCandidate {
                path,
                label,
                reachable,
                identities,
            }
        })
        .collect()
}

/// Pure decision core of auto-detect: given probe results, which candidate
/// wins. First preference to any candidate that actually reports identities
/// -- that's the only signal that distinguishes a real, usable agent from
/// one that merely exists (the reported bug this whole probing mechanism
/// exists to fix: a launchd or locked-vault agent that connects and answers
/// with zero keys). Falling back to "first that connects" when nothing has
/// identities keeps the locked-1Password-at-login case landing on 1Password
/// rather than Apple's agent, since `agent_candidate_paths` already ranks
/// 1Password ahead of the launchd socket.
fn choose_best(candidates: &[AgentCandidate]) -> Option<&AgentCandidate> {
    candidates
        .iter()
        .find(|candidate| candidate.identities.is_some_and(|count| count > 0))
        .or_else(|| candidates.iter().find(|candidate| candidate.reachable))
}

struct AgentResolution {
    path: String,
    identities: Option<u32>,
}

/// The operator override in Settings is still absolute -- probing never
/// second-guesses it -- but it's still probed (for its identity count only,
/// never to reject it) so the "connected but zero identities" warning
/// applies to an explicit path exactly as it does to an auto-detected one.
fn resolve_agent(settings: &Settings) -> Option<AgentResolution> {
    if !settings.ssh_auth_sock.trim().is_empty() {
        let path = settings.ssh_auth_sock.clone();
        let (_, identities) = probe_agent(&path);
        return Some(AgentResolution { path, identities });
    }
    let candidates = probe_candidates(agent_candidate_paths());
    choose_best(&candidates).map(|chosen| AgentResolution {
        path: chosen.path.clone(),
        identities: chosen.identities,
    })
}

// Only `resolve_agent` itself is used in production now (`run_loop` needs
// the identity count alongside the path); this thin wrapper survives purely
// to keep the test assertions below readable.
#[cfg(test)]
fn resolve_agent_socket(settings: &Settings) -> Option<String> {
    resolve_agent(settings).map(|resolution| resolution.path)
}

/// Probed once per resolution against every auto-detect candidate, and
/// exposed to the frontend via the `list_agent_sockets` command so the
/// Settings dropdown shows exactly what auto-detect would choose from --
/// `chosen` is computed here with the same `choose_best` `resolve_agent`
/// uses, rather than left for the frontend to re-derive, so there is only
/// ever one place that knows the ranking and the dropdown's "Auto-detect
/// (...)" label can never silently drift from what auto-detect actually
/// does.
#[derive(Serialize)]
pub struct AgentSocketList {
    pub candidates: Vec<AgentCandidate>,
    pub chosen: Option<String>,
}

pub fn list_agent_sockets() -> AgentSocketList {
    let candidates = probe_candidates(agent_candidate_paths());
    let chosen = choose_best(&candidates).map(|candidate| candidate.path.clone());
    AgentSocketList { candidates, chosen }
}

/// Connects to `path` and asks it for its identity count. This is a direct,
/// short-lived connection straight to the candidate agent -- entirely
/// separate from the worker's own muxed connection to the remote Mullion
/// session, and deliberately so: `src/worker/ssh-agent-protocol-v1.json`'s
/// `vectors` table gates *requests* flowing from a remote, muxed session
/// through `filter.mjs`; it has no entry for the `IDENTITIES_ANSWER`
/// response this probe reads, because that filter was never meant to see
/// one. Routing this probe through the worker would mean teaching that
/// filter about a response type it has no business inspecting. Don't "fix"
/// this by moving the probe into the worker.
fn probe_agent(path: &str) -> (bool, Option<u32>) {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        let Ok(mut stream) = UnixStream::connect(path) else {
            return (false, None);
        };
        let _ = stream.set_read_timeout(Some(AGENT_PROBE_TIMEOUT));
        let _ = stream.set_write_timeout(Some(AGENT_PROBE_TIMEOUT));
        (true, exchange_identities(&mut stream).ok())
    }
    #[cfg(windows)]
    {
        // `std::fs::File` (what a named pipe opens as) has no per-call read
        // timeout in std, unlike a Unix socket above -- so the whole
        // connect-and-exchange runs on a helper thread and this function
        // bounds ITS OWN wait with a channel timeout. That does not cancel
        // the thread itself: a pipe that accepts the connection and then
        // never answers leaves that thread permanently blocked in
        // `read_exact`. A real fix needs overlapped I/O (`ReadFile` +
        // `OVERLAPPED` + `CancelIoEx`) to actually cancel a stuck read,
        // which isn't available in std and isn't something this change can
        // add and then verify without a Windows machine -- so instead this
        // bounds the damage two ways: `PENDING_WINDOWS_AGENT_PROBES` caps
        // concurrent in-flight spawns (reclaimed the moment the caller
        // stops waiting, not just when the thread eventually finishes --
        // see `release_windows_probe_slot`), and `WINDOWS_PROBE_COOLDOWNS`
        // stops a specific wedged path from being re-spawned on every retry
        // (`resolve_agent` re-probes on every restart). Without the
        // cooldown, the concurrency cap alone would eventually saturate
        // from nothing but one already-known-bad path being retried over
        // and over, permanently shorting out probing for every OTHER
        // candidate too -- worse than the leak it was meant to bound.
        let now = Instant::now();
        {
            let mut cooldowns = WINDOWS_PROBE_COOLDOWNS
                .lock()
                .expect("windows probe cooldown mutex poisoned");
            if let Some(&until) = cooldowns.get(path) {
                if now < until {
                    return (false, None);
                }
                cooldowns.remove(path);
            }
        }
        if PENDING_WINDOWS_AGENT_PROBES.load(Ordering::SeqCst)
            >= MAX_CONCURRENT_WINDOWS_AGENT_PROBES
        {
            return (false, None);
        }
        PENDING_WINDOWS_AGENT_PROBES.fetch_add(1, Ordering::SeqCst);
        let released = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        let owned_path = path.to_owned();
        let thread_released = Arc::clone(&released);
        let thread_path = owned_path.clone();
        thread::spawn(move || {
            let result = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&owned_path)
                .map(|mut pipe| (true, exchange_identities(&mut pipe).ok()))
                .unwrap_or((false, None));
            let _ = tx.send(result);
            release_windows_probe_slot(&thread_released);
            // Whatever the outcome, the pipe just answered -- it isn't
            // wedged, so don't make a future probe wait out a cooldown that
            // no longer applies (this may run long after the caller below
            // already gave up and recorded one).
            WINDOWS_PROBE_COOLDOWNS
                .lock()
                .expect("windows probe cooldown mutex poisoned")
                .remove(&thread_path);
        });
        match rx.recv_timeout(AGENT_PROBE_TIMEOUT) {
            Ok(result) => result,
            Err(_) => {
                // The thread may still be blocked -- reclaim the
                // concurrency slot (it only ever bounded in-flight spawns,
                // not thread lifetime) and remember not to re-spawn against
                // this exact path again until the cooldown lapses, so a
                // genuinely wedged pipe costs one leaked thread per
                // cooldown window rather than one per retry.
                release_windows_probe_slot(&released);
                WINDOWS_PROBE_COOLDOWNS
                    .lock()
                    .expect("windows probe cooldown mutex poisoned")
                    .insert(path.to_owned(), now + WINDOWS_PROBE_COOLDOWN);
                (false, None)
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        (false, None)
    }
}

/// Sends `SSH_AGENTC_REQUEST_IDENTITIES` and returns the identity count from
/// a well-formed `SSH_AGENT_IDENTITIES_ANSWER`. Any other response --
/// including `SSH_AGENT_FAILURE`, a frame over the size cap, or the
/// connection closing mid-read -- is `Err`, which callers treat as "reachable,
/// identity count unknown" rather than propagating a specific reason; the
/// only thing this probe reports onward is a count.
fn exchange_identities(transport: &mut (impl Read + Write)) -> io::Result<u32> {
    transport.write_all(&REQUEST_IDENTITIES_FRAME)?;
    let mut len_buf = [0u8; 4];
    transport.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > MAX_AGENT_PROBE_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agent response frame outside the expected size",
        ));
    }
    let mut payload = vec![0u8; len];
    transport.read_exact(&mut payload)?;
    if payload.first().copied() != Some(SSH_AGENT_IDENTITIES_ANSWER) || payload.len() < 5 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a well-formed identities answer",
        ));
    }
    Ok(u32::from_be_bytes(payload[1..5].try_into().unwrap()))
}

fn worker_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("MULLION_WORKER_PATH") {
        return Ok(path.into());
    }
    let extension = if cfg!(windows) { ".exe" } else { "" };
    if let Ok(executable) = env::current_exe() {
        if let Some(parent) = executable.parent() {
            for candidate in [
                parent.join(format!("mullion-bridge-worker{extension}")),
                parent
                    .join("../Resources")
                    .join(format!("mullion-bridge-worker{extension}")),
            ] {
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }
    let development = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(format!(
            "mullion-bridge-worker-{}{extension}",
            env!("MULLION_TARGET_TRIPLE")
        ));
    if development.is_file() {
        Ok(development)
    } else {
        Err(format!(
            "Bundled bridge worker not found at {}",
            development.display()
        ))
    }
}

// Cap on what this returns to the UI, not on what's logged: a runaway
// stderr (e.g. a repeated panic loop) shouldn't be able to balloon the
// pairing window, but the full text is still worth having in the log file.
const WORKER_ERROR_DISPLAY_LIMIT: usize = 4000;

fn clean_worker_error(stderr: &[u8]) -> String {
    let value = String::from_utf8_lossy(stderr).trim().to_owned();
    if value.is_empty() {
        return "The bundled bridge worker failed without an error message".into();
    }
    log::error!("worker failed: {value}");
    if value.chars().count() <= WORKER_ERROR_DISPLAY_LIMIT {
        value
    } else {
        let mut truncated: String = value.chars().take(WORKER_ERROR_DISPLAY_LIMIT).collect();
        truncated.push_str("\n… (truncated, see log)");
        truncated
    }
}

fn should_reset_backoff(connected_at: Option<Instant>) -> bool {
    connected_at.is_some_and(|value| value.elapsed() >= HEALTHY_CONNECTION_RESET_AFTER)
}

/// Pure so this is testable with synthetic `Instant`s rather than a real
/// multi-minute sleep -- takes `now` explicitly (`Instant::now()` at the
/// call site otherwise) for the same reason.
fn should_restart_for_sustained_failure(unhealthy_since: Instant, now: Instant) -> bool {
    now.duration_since(unhealthy_since) >= SUSTAINED_FAILURE_RESTART_AFTER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore]
    fn child_process_fixture() {
        if env::var_os("MULLION_CHILD_PROCESS_FIXTURE").is_some() {
            thread::sleep(Duration::from_secs(60));
        }
    }

    fn sleeping_child() -> Child {
        Command::new(env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "supervisor::tests::child_process_fixture",
            ])
            .env("MULLION_CHILD_PROCESS_FIXTURE", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[test]
    fn stopping_a_child_synchronously_reaps_it() {
        let child = Mutex::new(Some(sleeping_child()));

        let status = stop_child(&child).expect("child should be stopped and reaped");

        assert!(!status.success());
        assert!(child.lock().unwrap().is_none());
    }

    #[test]
    fn unpair_stops_a_running_child_before_deleting_the_credential() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("unpair-stops-child");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        *supervisor.0.child.lock().unwrap() = Some(sleeping_child());

        supervisor.unpair().expect("unpair should succeed");

        assert!(supervisor.0.child.lock().unwrap().is_none());
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn unpair_deletes_the_credential_and_reports_unpaired() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("unpair");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        let credential_path = data_dir.join("worker/ssh-agent-bridge.json");
        fs::write(&credential_path, b"{}").unwrap();

        let status = supervisor.unpair().expect("unpair should succeed");

        assert_eq!(status.state, BridgeState::Unpaired);
        assert!(!credential_path.exists());
        assert!(!supervisor.0.desired.load(Ordering::SeqCst));
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn unpair_is_idempotent_when_already_unpaired() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("unpair-idempotent");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        // No credential file was ever written for this data dir.

        let status = supervisor
            .unpair()
            .expect("unpairing an already-unpaired bridge should succeed, not error");

        assert_eq!(status.state, BridgeState::Unpaired);
        let _ = fs::remove_dir_all(data_dir);
    }

    // Regression for the migration-durability warning Hermes review round 2
    // flagged on PR #47: a legacy-tool credential imported at startup but
    // never committed (because the bridge never reached a successful
    // "connected" event -- the exact shape of a paired-but-unreachable
    // install) left no marker file. Without unpair() finishing that pending
    // migration, the next launch's import_legacy_credential would silently
    // re-copy the same credential the user just unpaired right back into
    // the file unpair() just deleted, undoing the unpair on restart.
    #[test]
    #[cfg(not(windows))]
    fn unpair_completes_a_pending_legacy_migration_so_a_relaunch_cannot_resurrect_it() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("unpair-migration");
        fs::create_dir_all(&data_dir).unwrap();
        let legacy_home = test_data_dir("unpair-migration-legacy-home");
        let legacy_dir = legacy_home.join(".local/state/mullion");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(
            legacy_dir.join("ssh-agent-bridge.json"),
            br#"{"baseUrl":"https://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
        )
        .unwrap();
        let _home_guard = EnvVarGuard::set("HOME", legacy_home.to_str().unwrap());
        let _xdg_guard = EnvVarGuard::unset("XDG_STATE_HOME");

        let pending = crate::migration::import_legacy_credential(&data_dir)
            .expect("a valid legacy credential should produce a pending migration");
        // Simulates what lib.rs's setup() does: the pending migration sits
        // in managed state, uncommitted, because should_start was false
        // (or the app was closed) before a "connected" event ever fired.
        app.manage(crate::migration::MigrationState(Mutex::new(Some(pending))));
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();

        supervisor.unpair().expect("unpair should succeed");

        assert!(
            data_dir.join("legacy-migration.json").exists(),
            "unpair must finish the pending migration so a relaunch can't re-import it"
        );
        assert!(
            crate::migration::import_legacy_credential(&data_dir).is_none(),
            "a relaunch must not resurrect the credential the user just unpaired"
        );

        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&legacy_home);
    }

    // Regression for the round-4 note on PR #47: commit() collapses two
    // different failure modes into one Result, and disable_legacy_service()
    // running first (short-circuiting the marker write via `?`) means BOTH
    // failure modes leave the marker unwritten today. unpair() must not
    // assume a commit() failure is benign just because the credential was
    // already deleted -- it has to check whether the marker actually landed.
    #[test]
    #[cfg(not(windows))]
    fn unpair_reports_error_if_the_migration_marker_was_not_actually_written() {
        use std::os::unix::fs::PermissionsExt;

        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("unpair-migration-marker-fails");
        fs::create_dir_all(&data_dir).unwrap();
        let legacy_home = test_data_dir("unpair-migration-marker-fails-legacy-home");
        let legacy_dir = legacy_home.join(".local/state/mullion");
        fs::create_dir_all(&legacy_dir).unwrap();
        fs::write(
            legacy_dir.join("ssh-agent-bridge.json"),
            br#"{"baseUrl":"https://example.com","bridgeId":"123e4567-e89b-12d3-a456-426614174000","sessionId":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}"#,
        )
        .unwrap();
        let _home_guard = EnvVarGuard::set("HOME", legacy_home.to_str().unwrap());
        let _xdg_guard = EnvVarGuard::unset("XDG_STATE_HOME");

        let pending = crate::migration::import_legacy_credential(&data_dir)
            .expect("a valid legacy credential should produce a pending migration");
        app.manage(crate::migration::MigrationState(Mutex::new(Some(pending))));
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();

        // Make the marker write fail: on Linux, disable_legacy_service() is
        // a no-op Ok(()), so this is the only way commit() can fail here.
        let mut permissions = fs::metadata(&data_dir).unwrap().permissions();
        permissions.set_mode(0o500);
        fs::set_permissions(&data_dir, permissions).unwrap();

        let result = supervisor.unpair();

        // Restore write access before any cleanup that needs it.
        let mut permissions = fs::metadata(&data_dir).unwrap().permissions();
        permissions.set_mode(0o700);
        fs::set_permissions(&data_dir, permissions).unwrap();

        assert!(
            result.is_err(),
            "a commit() failure that left the marker unwritten must surface as an error, not a silent success"
        );
        assert_eq!(supervisor.status().state, BridgeState::Error);
        assert!(!data_dir.join("legacy-migration.json").exists());

        let _ = fs::remove_dir_all(&data_dir);
        let _ = fs::remove_dir_all(&legacy_home);
    }

    #[test]
    fn late_worker_events_are_ignored_once_desired_is_false() {
        // Regression for the race Hermes review flagged on PR #47: a stray
        // event from the just-killed worker's detached stdout reader
        // thread, dispatched after unpair()/pause() already set a terminal
        // status, must not overwrite it.
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("late-event-ignored");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.set_status(BridgeStatus::new(BridgeState::Unpaired, None));
        assert!(!supervisor.0.desired.load(Ordering::SeqCst));

        supervisor.handle_event(r#"{"type":"connected","base_url":"https://example.com"}"#);
        assert_eq!(supervisor.status().state, BridgeState::Unpaired);

        supervisor.handle_event(r#"{"type":"connect_failed","message":"connection error"}"#);
        assert_eq!(supervisor.status().state, BridgeState::Unpaired);

        supervisor.handle_event(r#"{"type":"dead_credential","message":"revoked"}"#);
        assert_eq!(supervisor.status().state, BridgeState::Unpaired);

        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn worker_events_still_apply_normally_while_desired() {
        // The gate above must not silently break the ordinary, non-race
        // path: while the supervisor still wants the bridge running, a
        // "connected" event must still land.
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("normal-event-applies");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.0.desired.store(true, Ordering::SeqCst);

        supervisor.handle_event(r#"{"type":"connected","base_url":"https://example.com"}"#);

        assert_eq!(supervisor.status().state, BridgeState::Connected);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn should_restart_for_sustained_failure_only_after_the_threshold_elapses() {
        let now = Instant::now();
        assert!(!should_restart_for_sustained_failure(now, now));
        assert!(!should_restart_for_sustained_failure(
            now - (SUSTAINED_FAILURE_RESTART_AFTER - Duration::from_secs(1)),
            now
        ));
        assert!(should_restart_for_sustained_failure(
            now - SUSTAINED_FAILURE_RESTART_AFTER,
            now
        ));
    }

    #[test]
    fn connect_failed_increments_the_visible_failure_counter_disconnected_does_not() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("failure-counter");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.0.desired.store(true, Ordering::SeqCst);

        supervisor.handle_event(r#"{"type":"connect_failed","message":"boom"}"#);
        assert_eq!(supervisor.status().consecutive_connect_failures, 1);

        supervisor.handle_event(r#"{"type":"disconnected"}"#);
        assert_eq!(
            supervisor.status().consecutive_connect_failures,
            1,
            "disconnected must not bump the connect-failure counter"
        );

        supervisor.handle_event(r#"{"type":"connect_failed","message":"boom again"}"#);
        assert_eq!(supervisor.status().consecutive_connect_failures, 2);

        supervisor.handle_event(r#"{"type":"connected","base_url":"https://example.com"}"#);
        assert_eq!(supervisor.status().consecutive_connect_failures, 0);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn a_fresh_connect_failure_streak_does_not_restart_the_worker() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("short-failure-streak");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.0.desired.store(true, Ordering::SeqCst);
        *supervisor.0.child.lock().unwrap() = Some(sleeping_child());

        supervisor.handle_event(r#"{"type":"connect_failed","message":"boom"}"#);

        assert!(
            supervisor.0.child.lock().unwrap().is_some(),
            "a failure streak still under the threshold must not restart the worker"
        );
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn a_sustained_connect_failure_streak_restarts_the_worker_and_re_arms() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("sustained-failure-restart");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.0.desired.store(true, Ordering::SeqCst);
        *supervisor.0.child.lock().unwrap() = Some(sleeping_child());
        // Simulates a streak that's already been unhealthy past the
        // threshold by the time this next connect_failed arrives.
        *supervisor.0.unhealthy_since.lock().unwrap() =
            Some(Instant::now() - SUSTAINED_FAILURE_RESTART_AFTER);
        supervisor
            .0
            .consecutive_connect_failures
            .store(9, Ordering::SeqCst);

        supervisor.handle_event(r#"{"type":"connect_failed","message":"still down"}"#);

        assert!(
            supervisor.0.child.lock().unwrap().is_none(),
            "a sustained failure must kill the wedged child so run_loop respawns it"
        );
        assert!(
            supervisor.0.unhealthy_since.lock().unwrap().is_none(),
            "the streak must re-arm after triggering a restart, not stay tripped"
        );
        assert_eq!(
            supervisor.status().consecutive_connect_failures,
            0,
            "the freshly restarted child must not inherit the pre-restart escalated count"
        );
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn pausing_resets_the_failure_streak_so_a_stale_duration_cannot_restart_on_resume() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("pause-resets-failure-streak");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.0.desired.store(true, Ordering::SeqCst);
        *supervisor.0.unhealthy_since.lock().unwrap() =
            Some(Instant::now() - SUSTAINED_FAILURE_RESTART_AFTER);
        supervisor
            .0
            .consecutive_connect_failures
            .store(3, Ordering::SeqCst);

        supervisor.pause();

        assert!(supervisor.0.unhealthy_since.lock().unwrap().is_none());
        assert_eq!(supervisor.status().consecutive_connect_failures, 0);
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn updater_shutdown_is_idempotent() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("idempotent");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.launch();

        supervisor.shutdown_for_update();
        supervisor.shutdown_for_update();

        assert!(supervisor.0.shutdown.load(Ordering::SeqCst));
        assert!(supervisor.0.thread.lock().unwrap().is_none());
        let _ = fs::remove_dir_all(data_dir);
    }

    #[test]
    fn blocking_shutdown_terminates_the_real_supervisor_loop() {
        let app = tauri::test::mock_app();
        let data_dir = test_data_dir("loop-termination");
        let supervisor = Supervisor::new(app.handle().clone(), data_dir.clone()).unwrap();
        supervisor.launch();
        assert!(supervisor.0.thread.lock().unwrap().is_some());

        supervisor.shutdown_for_update();

        assert!(supervisor.0.thread.lock().unwrap().is_none());
        let _ = fs::remove_dir_all(data_dir);
    }

    fn test_data_dir(name: &str) -> PathBuf {
        let path = env::temp_dir().join(format!(
            "mullion-helper-supervisor-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        path
    }

    /// A `HOME` short enough that `<home>/Library/Group Containers/
    /// 2BUA8C4S2C.com.1password/t/agent.sock` still fits inside
    /// `sockaddr_un::sun_path` (108 bytes on Linux, 104 on BSD/macOS) when a
    /// test needs to really `bind()`/`connect()` a Unix socket there.
    /// `test_data_dir`'s descriptive names are far too long for that -- this
    /// is deliberately terse instead, hard-coded to `/tmp` rather than
    /// `env::temp_dir()` since `$TMPDIR` on macOS is itself often 40+ bytes,
    /// which alone can blow the budget.
    #[cfg(unix)]
    fn short_socket_home(tag: &str) -> PathBuf {
        let path = PathBuf::from("/tmp").join(format!("mh{tag}{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        path
    }

    #[test]
    fn explicit_socket_wins() {
        // Deliberately points at nothing -- the whole point of this test is
        // that an explicit override is trusted absolutely, even when the
        // probe behind it can't reach anything.
        let settings = Settings {
            ssh_auth_sock: "/tmp/custom-agent.sock-does-not-exist".into(),
            insecure: false,
            launch_at_login: false,
        };
        assert_eq!(
            resolve_agent_socket(&settings).as_deref(),
            Some("/tmp/custom-agent.sock-does-not-exist")
        );
    }

    #[test]
    fn recognizes_the_macos_launchd_socket_shape() {
        // Confirmed via `launchctl getenv SSH_AUTH_SOCK` on the reporting
        // machine -- the value the earlier, prefix-based version of
        // `is_macos_launchd_socket` failed to match (see its doc comment).
        assert!(is_macos_launchd_socket(
            "/var/run/com.apple.launchd.oLcNuPYLZu/Listeners"
        ));
        // The directory this was originally (incorrectly) assumed to
        // always live under. Also still a launchd socket -- keep both
        // shapes covered since which one macOS hands out isn't something
        // this code controls.
        assert!(is_macos_launchd_socket(
            "/private/tmp/com.apple.launchd.ABC123xyz/Listeners"
        ));
        // A non-launchd SSH_AUTH_SOCK must not be misclassified — that
        // would defer a perfectly good agent for no reason.
        assert!(!is_macos_launchd_socket("/tmp/ssh-AbCdEf/agent.12345"));
        assert!(!is_macos_launchd_socket(
            "/Users/me/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"
        ));
        // A path that merely contains the substring, rather than having it
        // as the actual parent directory segment, must not match.
        assert!(!is_macos_launchd_socket(
            "/tmp/not-com.apple.launchd.fake/Listeners"
        ));
        assert!(!is_macos_launchd_socket(
            "/var/run/com.apple.launchd.oLcNuPYLZu/NotListeners"
        ));
        // A trailing slash must not defeat the match — a false negative
        // here means trusting an empty agent over 1Password again.
        assert!(is_macos_launchd_socket(
            "/var/run/com.apple.launchd.oLcNuPYLZu/Listeners/"
        ));
    }

    const LAUNCHD_SOCK: &str = "/var/run/com.apple.launchd.oLcNuPYLZu/Listeners";

    fn candidate(
        path: &str,
        label: &'static str,
        reachable: bool,
        identities: Option<u32>,
    ) -> AgentCandidate {
        AgentCandidate {
            path: path.to_owned(),
            label,
            reachable,
            identities,
        }
    }

    // Pure regression tests for the reported bug, now expressed on
    // `choose_best` over literal `AgentCandidate` values instead of raw
    // existence flags -- selection is driven by probed identity counts, not
    // by what merely exists on disk.
    #[test]
    fn choose_best_prefers_any_candidate_with_identities_over_a_merely_reachable_one() {
        let candidates = vec![
            candidate(LAUNCHD_SOCK, "macOS login agent", true, Some(0)),
            candidate("/1p", "1Password", true, Some(3)),
        ];
        assert_eq!(
            choose_best(&candidates).map(|c| c.path.as_str()),
            Some("/1p")
        );
    }

    #[test]
    fn choose_best_falls_back_to_first_reachable_when_nothing_has_identities() {
        // The reported configuration: a locked (connectable, zero-identity)
        // 1Password ranked ahead of the (also zero-identity) launchd agent
        // must still win, purely by list order, once neither has identities.
        let candidates = vec![
            candidate("/1p", "1Password", true, Some(0)),
            candidate(LAUNCHD_SOCK, "macOS login agent", true, Some(0)),
        ];
        assert_eq!(
            choose_best(&candidates).map(|c| c.path.as_str()),
            Some("/1p")
        );
    }

    #[test]
    fn choose_best_skips_unreachable_candidates_entirely() {
        let candidates = vec![
            candidate("/dead", "SSH_AUTH_SOCK", false, None),
            candidate("/1p", "1Password", true, Some(0)),
        ];
        assert_eq!(
            choose_best(&candidates).map(|c| c.path.as_str()),
            Some("/1p")
        );
    }

    #[test]
    fn choose_best_returns_none_when_nothing_is_reachable() {
        let candidates = vec![candidate("/dead", "SSH_AUTH_SOCK", false, None)];
        assert!(choose_best(&candidates).is_none());
    }

    // `agent_candidate_paths`/`resolve_agent` read process-global env vars
    // (`SSH_AUTH_SOCK`, `HOME`), and `cargo test` runs tests in parallel by
    // default — every test below that touches either takes this lock so
    // runs can't interleave and observe a value neither of them set. Any
    // future test touching these two env vars must take it as well.
    static AGENT_ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = env::var(key).ok();
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[test]
    #[cfg(not(windows))]
    fn candidate_list_ranks_1password_ahead_of_the_launchd_socket() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", LAUNCHD_SOCK);
        // agent_candidate_paths doesn't check existence, so any HOME value
        // is enough to make the two 1Password paths appear in the list.
        let _home_guard = EnvVarGuard::set("HOME", "/nonexistent-home-for-tests");

        let labels: Vec<&str> = agent_candidate_paths()
            .iter()
            .map(|(_, label)| *label)
            .collect();
        assert_eq!(
            labels,
            vec!["1Password", "1Password (legacy path)", "macOS login agent"]
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn candidate_list_omits_the_launchd_socket_when_home_is_unset() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", LAUNCHD_SOCK);
        let _home_guard = EnvVarGuard::unset("HOME");

        let candidates = agent_candidate_paths();
        let paths: Vec<&str> = candidates.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, vec![LAUNCHD_SOCK]);
    }

    #[test]
    #[cfg(not(windows))]
    fn candidate_list_puts_a_real_non_launchd_sock_first() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", "/tmp/ssh-AbCdEf/agent.12345");
        let _home_guard = EnvVarGuard::set("HOME", "/nonexistent-home-for-tests");

        let candidates = agent_candidate_paths();
        assert_eq!(
            candidates.first().map(|(path, _)| path.as_str()),
            Some("/tmp/ssh-AbCdEf/agent.12345")
        );
        assert_eq!(
            candidates.last().map(|(_, label)| *label),
            Some("1Password (legacy path)")
        );
    }

    /// Binds a real `UnixListener` at `path` (synchronously, so it's ready
    /// to accept the moment this returns) and answers the first connection
    /// with either a well-formed `IDENTITIES_ANSWER` carrying
    /// `respond_with_identities` identities, or -- if `None` -- accepts the
    /// connection and then closes it without replying, exercising the
    /// "reachable but the exchange itself doesn't pan out" path.
    #[cfg(unix)]
    fn spawn_fake_agent(path: &Path, respond_with_identities: Option<u32>) {
        let listener = std::os::unix::net::UnixListener::bind(path).unwrap();
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                if let Some(count) = respond_with_identities {
                    let mut request = [0u8; 5];
                    if stream.read_exact(&mut request).is_ok() {
                        let mut payload = vec![SSH_AGENT_IDENTITIES_ANSWER];
                        payload.extend_from_slice(&count.to_be_bytes());
                        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
                        frame.extend_from_slice(&payload);
                        let _ = stream.write_all(&frame);
                    }
                }
            }
        });
    }

    #[test]
    #[cfg(unix)]
    fn exchange_identities_reads_a_well_formed_answer() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        thread::spawn(move || {
            let mut server = server;
            let mut request = [0u8; 5];
            server.read_exact(&mut request).unwrap();
            assert_eq!(request, REQUEST_IDENTITIES_FRAME);
            let mut payload = vec![SSH_AGENT_IDENTITIES_ANSWER];
            payload.extend_from_slice(&7u32.to_be_bytes());
            let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
            frame.extend_from_slice(&payload);
            server.write_all(&frame).unwrap();
        });
        assert_eq!(exchange_identities(&mut client).unwrap(), 7);
    }

    #[test]
    #[cfg(unix)]
    fn exchange_identities_rejects_a_non_identities_answer() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        thread::spawn(move || {
            let mut server = server;
            let mut request = [0u8; 5];
            server.read_exact(&mut request).unwrap();
            // SSH_AGENT_FAILURE (type 5), no count -- what a real agent
            // sends for a rejected/unsupported request.
            let _ = server.write_all(&[0, 0, 0, 1, 5]);
        });
        assert!(exchange_identities(&mut client).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn exchange_identities_errors_when_the_connection_closes_without_replying() {
        let (mut client, server) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(server);
        assert!(exchange_identities(&mut client).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn probe_agent_reports_unreachable_for_a_path_nothing_is_listening_on() {
        let path = test_data_dir("agent-probe-unreachable").with_extension("sock");
        assert_eq!(probe_agent(path.to_str().unwrap()), (false, None));
    }

    #[test]
    #[cfg(unix)]
    fn probe_agent_reports_identities_from_a_real_listener() {
        let path = test_data_dir("agent-probe-identities").with_extension("sock");
        spawn_fake_agent(&path, Some(3));
        assert_eq!(probe_agent(path.to_str().unwrap()), (true, Some(3)));
        let _ = fs::remove_file(&path);
    }

    #[test]
    #[cfg(unix)]
    fn probe_agent_reports_reachable_with_unknown_identities_when_the_agent_never_replies() {
        let path = test_data_dir("agent-probe-silent").with_extension("sock");
        spawn_fake_agent(&path, None);
        assert_eq!(probe_agent(path.to_str().unwrap()), (true, None));
        let _ = fs::remove_file(&path);
    }

    // End-to-end `resolve_agent`/`resolve_agent_socket` tests: real
    // `UnixListener`s standing in for 1Password and the launchd agent, so
    // these exercise probing and `choose_best` together exactly as
    // `run_loop` does, not just the pure ranking proven above.
    #[test]
    #[cfg(not(windows))]
    fn resolve_agent_socket_picks_the_only_candidate_reporting_identities() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let home = short_socket_home("a");
        fs::create_dir_all(&home).unwrap();
        let onepassword_sock =
            home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock");
        fs::create_dir_all(onepassword_sock.parent().unwrap()).unwrap();
        spawn_fake_agent(&onepassword_sock, Some(2));
        let real_sock = home.join("real-agent.sock");
        spawn_fake_agent(&real_sock, Some(0));
        let _home_guard = EnvVarGuard::set("HOME", home.to_str().unwrap());
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", real_sock.to_str().unwrap());

        let settings = Settings::default();
        assert_eq!(
            resolve_agent_socket(&settings).as_deref(),
            onepassword_sock.to_str()
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    #[cfg(not(windows))]
    fn resolve_agent_socket_reported_configuration_picks_1password_over_a_locked_vault_and_launchd()
    {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let home = short_socket_home("b");
        fs::create_dir_all(&home).unwrap();
        let onepassword_sock =
            home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock");
        fs::create_dir_all(onepassword_sock.parent().unwrap()).unwrap();
        // A locked vault: connects, reports zero identities -- must still
        // beat the launchd agent purely by candidate-list order.
        spawn_fake_agent(&onepassword_sock, Some(0));
        let _home_guard = EnvVarGuard::set("HOME", home.to_str().unwrap());
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", LAUNCHD_SOCK);

        let settings = Settings::default();
        assert_eq!(
            resolve_agent_socket(&settings).as_deref(),
            onepassword_sock.to_str()
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    #[cfg(not(windows))]
    fn resolve_agent_socket_returns_none_when_no_candidate_is_reachable() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let home = short_socket_home("c");
        let _home_guard = EnvVarGuard::set("HOME", home.to_str().unwrap());
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", "/tmp/dead-agent-sock-for-tests");

        let settings = Settings::default();
        assert_eq!(resolve_agent_socket(&settings), None);
    }

    #[test]
    fn status_names_are_stable() {
        assert_eq!(
            serde_json::to_string(&BridgeState::NeedsPairing).unwrap(),
            "\"needs_pairing\""
        );
    }
    #[test]
    fn backoff_only_resets_after_a_stable_connection() {
        assert!(!should_reset_backoff(None));
        assert!(!should_reset_backoff(Some(Instant::now())));
        assert!(should_reset_backoff(Some(
            Instant::now() - HEALTHY_CONNECTION_RESET_AFTER
        )));
    }

    #[test]
    fn clean_worker_error_falls_back_when_stderr_is_empty() {
        assert_eq!(
            clean_worker_error(b"   \n  "),
            "The bundled bridge worker failed without an error message"
        );
    }

    #[test]
    fn clean_worker_error_passes_short_text_through_untouched() {
        let stderr = b"line one\nline two\n";
        assert_eq!(clean_worker_error(stderr), "line one\nline two");
    }

    #[test]
    fn clean_worker_error_truncates_and_marks_long_output() {
        let stderr = "x".repeat(WORKER_ERROR_DISPLAY_LIMIT + 500);
        let cleaned = clean_worker_error(stderr.as_bytes());
        assert!(cleaned.starts_with(&"x".repeat(WORKER_ERROR_DISPLAY_LIMIT)));
        assert!(cleaned.ends_with("\n… (truncated, see log)"));
        assert_eq!(
            cleaned.chars().count(),
            WORKER_ERROR_DISPLAY_LIMIT + "\n… (truncated, see log)".chars().count()
        );
    }
}
