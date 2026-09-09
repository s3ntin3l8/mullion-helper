use crate::migration::MigrationState;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread,
    time::Duration,
};
use tauri::{AppHandle, Emitter, Manager};

const WINDOWS_AGENT_PIPE: &str = r"\\.\pipe\openssh-ssh-agent";

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

struct Inner {
    app: AppHandle,
    worker: PathBuf,
    data_dir: PathBuf,
    desired: AtomicBool,
    shutdown: AtomicBool,
    child: Mutex<Option<Child>>,
    status: Mutex<BridgeStatus>,
    settings: Mutex<Settings>,
}

#[derive(Clone)]
pub struct Supervisor(Arc<Inner>);

impl Supervisor {
    pub fn new(app: AppHandle, data_dir: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(data_dir.join("worker")).map_err(|error| error.to_string())?;
        let settings = read_settings(&data_dir);
        Ok(Self(Arc::new(Inner {
            app,
            worker: worker_path()?,
            data_dir,
            desired: AtomicBool::new(false),
            shutdown: AtomicBool::new(false),
            child: Mutex::new(None),
            status: Mutex::new(BridgeStatus::new(BridgeState::Unpaired, None)),
            settings: Mutex::new(settings),
        })))
    }

    pub fn launch(self) {
        thread::spawn(move || self.run_loop());
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
            self.kill_child();
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
        self.kill_child();
        self.set_status(BridgeStatus::new(BridgeState::Paused, None));
        self.status()
    }

    pub fn shutdown(&self) {
        self.0.shutdown.store(true, Ordering::SeqCst);
        self.0.desired.store(false, Ordering::SeqCst);
        self.kill_child();
    }

    pub fn inspect(&self) -> Result<bool, String> {
        Ok(self.inspect_detail()?.paired)
    }

    pub fn pair(&self, payload: &str) -> Result<BridgeStatus, String> {
        if payload.trim().is_empty() || payload.len() > 8192 {
            return Err("The pairing payload is empty or too large.".into());
        }
        self.0.desired.store(false, Ordering::SeqCst);
        self.kill_child();
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
                thread::spawn(
                    move || for _line in BufReader::new(stderr).lines().map_while(Result::ok) {},
                );
            }
            loop {
                if !self.0.desired.load(Ordering::SeqCst) || self.0.shutdown.load(Ordering::SeqCst)
                {
                    self.kill_child();
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
            *self.0.child.lock().expect("child mutex poisoned") = None;
            if self.0.desired.load(Ordering::SeqCst) && !self.0.shutdown.load(Ordering::SeqCst) {
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
                            self.kill_child();
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
            Some("dead_credential") | Some("renewal_rejected") => {
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
        let mut command = Command::new(&self.0.worker);
        command.env("MULLION_HELPER_STATE_DIR", self.0.data_dir.join("worker"));
        command
    }

    fn kill_child(&self) {
        if let Ok(mut guard) = self.0.child.lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.kill();
            }
        }
    }

    fn set_status(&self, status: BridgeStatus) {
        *self.0.status.lock().expect("status mutex poisoned") = status.clone();
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

fn clean_worker_error(stderr: &[u8]) -> String {
    let value = String::from_utf8_lossy(stderr).trim().to_owned();
    if value.is_empty() {
        "The bundled bridge worker failed without an error message".into()
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
