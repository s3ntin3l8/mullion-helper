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
            Some("disconnected") | Some("connect_failed") => {
                let mut status = self.status();
                status.state = BridgeState::Reconnecting;
                status.detail = event
                    .get("message")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
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

fn resolve_agent_socket(settings: &Settings) -> Option<String> {
    if !settings.ssh_auth_sock.trim().is_empty() {
        return Some(settings.ssh_auth_sock.clone());
    }
    if let Ok(value) = env::var("SSH_AUTH_SOCK") {
        if cfg!(windows) || Path::new(&value).exists() {
            return Some(value);
        }
    }
    if cfg!(windows) {
        return Some(WINDOWS_AGENT_PIPE.into());
    }
    let home = env::var_os("HOME").map(PathBuf::from)?;
    let candidates = [
        home.join("Library/Group Containers/2BUA8C4S2C.com.1password/t/agent.sock"),
        home.join(".1password/agent.sock"),
    ];
    candidates
        .into_iter()
        .find(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
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
