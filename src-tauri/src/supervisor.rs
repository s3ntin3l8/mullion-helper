use crate::{headless_process, migration::MigrationState, tray_status::TrayStatus};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Emitter, Manager, Runtime, Wry};

const WINDOWS_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";
const HEALTHY_CONNECTION_RESET_AFTER: Duration = Duration::from_secs(30);

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
                    self.set_status(BridgeStatus::new(BridgeState::Error, Some(error.clone())));
                    return Err(error);
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
            let socket = match resolve_agent_socket(&self.settings()) {
                Some(value) => value,
                None => {
                    self.set_status(BridgeStatus::new(BridgeState::AgentUnavailable, Some("No SSH agent socket was found. Open your SSH agent or configure its socket in Settings.".into())));
                    self.interruptible_sleep(Duration::from_secs(3));
                    continue;
                }
            };
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
                let mut status = self.status();
                status.state = BridgeState::Reconnecting;
                status.detail = message.map(str::to_owned);
                status.updated_at = Utc::now().to_rfc3339();
                self.set_status(status);
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

    fn set_status(&self, status: BridgeStatus) {
        *self.0.status.lock().expect("status mutex poisoned") = status.clone();
        if let Some(tray_status) = self.0.app.try_state::<TrayStatus<R>>() {
            tray_status.update(&status);
        }
        let _ = self.0.app.emit("bridge-status", status);
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

/// Pure decision core of `resolve_agent_socket`'s auto-detect path, kept
/// separate so the ranking itself is testable with plain values — no env
/// vars, no filesystem, and in particular no need to fake a real launchd
/// socket file just to exercise the deferral (which isn't even possible to
/// do safely and portably in a unit test on a non-macOS CI runner).
///
/// `env_sock_exists` and `first_existing_1password` are pre-resolved by the
/// caller (real `Path::exists()` checks); this function only decides
/// precedence. Order: a non-launchd, existing `SSH_AUTH_SOCK` wins; else the
/// first existing 1Password candidate; else the (launchd) `SSH_AUTH_SOCK`
/// anyway if it exists — connecting to *something* beats reporting
/// `AgentUnavailable`, matching the Windows branch's unconditional trust of
/// `SSH_AUTH_SOCK` just above this function's call site. Please don't
/// "simplify" this last fallback away — it's intentional, see the tests.
fn pick_auto_detected_socket(
    env_sock: Option<&str>,
    env_sock_exists: bool,
    first_existing_1password: Option<&str>,
) -> Option<String> {
    let env_is_launchd = env_sock.is_some_and(is_macos_launchd_socket);
    if env_sock_exists && !env_is_launchd {
        return env_sock.map(str::to_owned);
    }
    if let Some(path) = first_existing_1password {
        return Some(path.to_owned());
    }
    env_sock.filter(|_| env_sock_exists).map(str::to_owned)
}

fn resolve_agent_socket(settings: &Settings) -> Option<String> {
    if !settings.ssh_auth_sock.trim().is_empty() {
        return Some(settings.ssh_auth_sock.clone());
    }
    let env_sock = env::var("SSH_AUTH_SOCK").ok();
    if cfg!(windows) {
        // Unlike the Unix branch below, this is a deliberate unconditional
        // trust with no existence check — an out-of-scope asymmetry left
        // for a follow-up PR alongside probing agents for real identities.
        return env_sock.or_else(|| Some(WINDOWS_AGENT_PIPE.into()));
    }
    let env_sock_exists = env_sock
        .as_deref()
        .is_some_and(|value| Path::new(value).exists());
    let first_existing_1password = env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|home| {
            [
                home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"),
                home.join(".1password/agent.sock"),
            ]
            .into_iter()
            .find(|path| path.exists())
        })
        .map(|path| path.to_string_lossy().into_owned());
    pick_auto_detected_socket(
        env_sock.as_deref(),
        env_sock_exists,
        first_existing_1password.as_deref(),
    )
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

    #[test]
    fn explicit_socket_wins() {
        let settings = Settings {
            ssh_auth_sock: "/tmp/custom-agent.sock".into(),
            insecure: false,
            launch_at_login: false,
        };
        assert_eq!(
            resolve_agent_socket(&settings).as_deref(),
            Some("/tmp/custom-agent.sock")
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

    // The real regression tests for the reported bug: `pick_auto_detected_
    // socket` is pure, so these use plain literals rather than faking a
    // real launchd socket file — which isn't even possible to do portably
    // in a test that might run on Linux CI. This is the exact value
    // confirmed via `launchctl getenv SSH_AUTH_SOCK` on the reporting
    // machine, not a guessed shape.
    const LAUNCHD_SOCK: &str = "/var/run/com.apple.launchd.oLcNuPYLZu/Listeners";
    const ONEPASSWORD_SOCK: &str =
        "/Users/me/Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock";

    #[test]
    fn reported_bug_launchd_socket_present_defers_to_1password() {
        assert_eq!(
            pick_auto_detected_socket(Some(LAUNCHD_SOCK), true, Some(ONEPASSWORD_SOCK)),
            Some(ONEPASSWORD_SOCK.to_owned())
        );
    }

    #[test]
    fn a_real_non_launchd_sock_still_wins_over_1password() {
        assert_eq!(
            pick_auto_detected_socket(
                Some("/tmp/ssh-AbCdEf/agent.12345"),
                true,
                Some(ONEPASSWORD_SOCK)
            ),
            Some("/tmp/ssh-AbCdEf/agent.12345".to_owned())
        );
    }

    #[test]
    fn launchd_socket_is_the_last_resort_fallback() {
        // No 1Password candidate found (e.g. HOME unset, or 1Password not
        // installed): fall back to the launchd socket rather than
        // AgentUnavailable — connecting to something beats nothing.
        assert_eq!(
            pick_auto_detected_socket(Some(LAUNCHD_SOCK), true, None),
            Some(LAUNCHD_SOCK.to_owned())
        );
    }

    #[test]
    fn a_stale_env_sock_that_does_not_exist_is_ignored() {
        // SSH_AUTH_SOCK set but dangling (e.g. dead forwarded socket): fall
        // straight through to 1Password rather than the last-resort branch,
        // matching pre-existing behavior for a non-launchd dangling socket.
        assert_eq!(
            pick_auto_detected_socket(Some("/tmp/dead.sock"), false, Some(ONEPASSWORD_SOCK)),
            Some(ONEPASSWORD_SOCK.to_owned())
        );
        assert_eq!(
            pick_auto_detected_socket(Some("/tmp/dead.sock"), false, None),
            None
        );
    }

    #[test]
    fn no_env_sock_falls_through_to_1password_or_none() {
        assert_eq!(
            pick_auto_detected_socket(None, false, Some(ONEPASSWORD_SOCK)),
            Some(ONEPASSWORD_SOCK.to_owned())
        );
        assert_eq!(pick_auto_detected_socket(None, false, None), None);
    }

    // `resolve_agent_socket` auto-detect reads process-global env vars
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

    fn write_fake_1password_socket(home: &Path) -> PathBuf {
        let socket = home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock");
        fs::create_dir_all(socket.parent().unwrap()).unwrap();
        fs::write(&socket, b"").unwrap();
        socket
    }

    // These two exercise `resolve_agent_socket`'s *wiring* — reading
    // `SSH_AUTH_SOCK`/`HOME` and building the real 1Password candidate
    // paths — on top of the ranking already proven purely above. (A
    // wiring test can't itself fake a launchd socket that `exists()`
    // without writing to the real `/private/tmp`, which isn't possible
    // portably in a unit test — that's exactly why the ranking has its own
    // pure tests instead.) Both take `AGENT_ENV_LOCK` since they mutate
    // process-global env vars and `cargo test` runs in parallel by default.
    #[test]
    #[cfg(not(windows))]
    fn resolve_agent_socket_finds_1password_via_home_when_env_sock_is_unset() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let home = test_data_dir("agent-home-wiring");
        fs::create_dir_all(&home).unwrap();
        let onepassword_sock = write_fake_1password_socket(&home);
        let _home_guard = EnvVarGuard::set("HOME", home.to_str().unwrap());
        let _sock_guard = EnvVarGuard::unset("SSH_AUTH_SOCK");

        let settings = Settings::default();
        assert_eq!(
            resolve_agent_socket(&settings).as_deref(),
            onepassword_sock.to_str()
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    #[cfg(not(windows))]
    fn a_real_ssh_auth_sock_still_wins_over_1password() {
        let _lock = AGENT_ENV_LOCK.lock().unwrap();
        let home = test_data_dir("agent-home-real-sock");
        fs::create_dir_all(&home).unwrap();
        write_fake_1password_socket(&home);
        let _home_guard = EnvVarGuard::set("HOME", home.to_str().unwrap());
        // A real, existing, non-launchd-shaped SSH_AUTH_SOCK (e.g. a
        // forwarded agent socket) must not be second-guessed just because
        // 1Password also happens to be present.
        let real_sock = home.join("real-agent.sock");
        fs::write(&real_sock, b"").unwrap();
        let _sock_guard = EnvVarGuard::set("SSH_AUTH_SOCK", real_sock.to_str().unwrap());

        let settings = Settings::default();
        assert_eq!(
            resolve_agent_socket(&settings).as_deref(),
            real_sock.to_str()
        );

        let _ = fs::remove_dir_all(&home);
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
