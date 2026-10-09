use std::sync::{Arc, Mutex};
use tauri::{AppHandle, Manager, WebviewWindowBuilder};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Phase {
    #[default]
    Absent,
    Creating,
    Open,
    Destroying,
}

#[derive(Default)]
struct Lifecycle {
    phase: Phase,
    generation: u32,
    reopen: bool,
    pending_close: bool,
    operation: Option<u32>,
    next_operation: u32,
    shutting_down: bool,
}

impl Lifecycle {
    fn finish_creation(&mut self, generation: u32) -> bool {
        if self.phase != Phase::Creating || self.generation != generation || self.shutting_down {
            return false;
        }
        self.phase = Phase::Open;
        true
    }

    fn request_open(&mut self) -> bool {
        if self.shutting_down {
            return false;
        }
        self.pending_close = false;
        match self.phase {
            Phase::Absent => {
                self.generation = self.generation.wrapping_add(1);
                self.phase = Phase::Creating;
                true
            }
            Phase::Open => true,
            Phase::Creating => false,
            Phase::Destroying => {
                self.reopen = true;
                false
            }
        }
    }

    fn request_close(&mut self) -> bool {
        if self.phase != Phase::Open {
            return false;
        }
        if self.operation.is_some() {
            self.pending_close = true;
            false
        } else {
            self.phase = Phase::Destroying;
            true
        }
    }

    fn begin_operation(&mut self) -> Result<u32, String> {
        if self.shutting_down || self.phase != Phase::Open || self.pending_close {
            return Err("The window is closing. Reopen it to try again.".into());
        }
        if self.operation.is_some() {
            return Err("Another operation is still running.".into());
        }
        self.next_operation = self.next_operation.wrapping_add(1);
        self.operation = Some(self.next_operation);
        Ok(self.next_operation)
    }

    fn end_operation(&mut self, operation: u32, success: bool) -> bool {
        // Ignore a completion from a webview that was unexpectedly destroyed
        // and replaced while its command was in flight.
        if self.operation != Some(operation) {
            return false;
        }
        self.operation = None;
        let close = std::mem::take(&mut self.pending_close) && success;
        close && self.request_close()
    }

    fn destroyed(&mut self) -> bool {
        self.phase = Phase::Absent;
        self.operation = None;
        self.pending_close = false;
        std::mem::take(&mut self.reopen) && !self.shutting_down
    }

    fn destroy_failed(&mut self, generation: u32) {
        if self.generation == generation && self.phase == Phase::Destroying {
            self.phase = Phase::Open;
            self.reopen = false;
        }
    }
}

#[derive(Clone, Default)]
pub struct MainWindow(Arc<Mutex<Lifecycle>>);

impl MainWindow {
    pub fn open(&self, app: &AppHandle) {
        let (create, generation) = {
            let mut state = self.0.lock().expect("window mutex poisoned");
            let create = state.phase == Phase::Absent;
            if !state.request_open() {
                return;
            }
            (create, state.generation)
        };
        if !create {
            let lifecycle = self.clone();
            let handle = app.clone();
            if let Err(error) = app.run_on_main_thread(move || {
                let focus = {
                    let state = lifecycle.0.lock().expect("window mutex poisoned");
                    state.phase == Phase::Open && !state.shutting_down
                };
                if focus {
                    if let Some(window) = handle.get_webview_window("main") {
                        show(&window);
                    }
                }
            }) {
                log::warn!("could not schedule window focus: {error}");
            }
            return;
        }
        let lifecycle = self.clone();
        let app = app.clone();
        // WebviewWindowBuilder deadlocks on Windows in synchronous event
        // handlers. Never hold our mutex while building or calling Tauri.
        std::thread::spawn(move || {
            let config = app
                .config()
                .app
                .windows
                .iter()
                .find(|config| config.label == "main")
                .expect("main window configuration missing");
            let result =
                WebviewWindowBuilder::from_config(&app, config).and_then(|builder| builder.build());
            match result {
                Ok(window) => {
                    // Publish Open and show together on the UI event loop.
                    // Otherwise a second activation can show/close the window
                    // before this thread resumes, and a stale show could
                    // restore macOS Dock visibility after destruction.
                    if let Err(error) = app.run_on_main_thread(move || {
                        let show_window = lifecycle
                            .0
                            .lock()
                            .expect("window mutex poisoned")
                            .finish_creation(generation);
                        if show_window {
                            show(&window);
                        } else {
                            let _ = window.destroy();
                        }
                    }) {
                        log::error!("could not schedule the main window: {error}");
                    }
                }
                Err(error) => {
                    let mut state = lifecycle.0.lock().expect("window mutex poisoned");
                    if state.generation == generation && state.phase == Phase::Creating {
                        state.phase = Phase::Absent;
                    }
                    log::error!("could not create the main window: {error}");
                }
            }
        });
    }

    pub fn close(&self, app: &AppHandle) {
        let destroy = self
            .0
            .lock()
            .expect("window mutex poisoned")
            .request_close();
        if destroy {
            self.destroy(app);
        }
    }

    pub fn begin_operation(&self) -> Result<u32, String> {
        self.0
            .lock()
            .expect("window mutex poisoned")
            .begin_operation()
    }

    pub fn end_operation(&self, app: &AppHandle, operation: u32, success: bool) {
        let destroy = self
            .0
            .lock()
            .expect("window mutex poisoned")
            .end_operation(operation, success);
        if destroy {
            self.destroy(app);
        }
    }

    fn destroy(&self, app: &AppHandle) {
        let generation = self.0.lock().expect("window mutex poisoned").generation;
        // destroy() already dispatches through Tauri's event-loop proxy.
        // Capture this exact window now: a worker's later lookup by label
        // could target a replacement after an unexpected native destruction.
        // Only the actual Destroyed event may reset the lifecycle.
        if let Some(window) = app.get_webview_window("main") {
            if let Err(error) = window.destroy() {
                self.0
                    .lock()
                    .expect("window mutex poisoned")
                    .destroy_failed(generation);
                log::warn!("could not destroy the main window: {error}");
            }
        }
    }

    pub fn on_destroyed(&self, app: &AppHandle) {
        let reopen = self.0.lock().expect("window mutex poisoned").destroyed();
        // Revert the explicit Regular policy used while the window is open;
        // Accessory alone doesn't reliably undo AppKit's Dock promotion.
        #[cfg(target_os = "macos")]
        if let Err(error) = app.set_activation_policy(tauri::ActivationPolicy::Accessory) {
            log::warn!("could not switch back to the Accessory activation policy: {error}");
        }
        if reopen {
            self.open(app);
        }
    }

    pub fn shutdown(&self) {
        self.0.lock().expect("window mutex poisoned").shutting_down = true;
    }
}

fn show(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "macos")]
    if let Err(error) = window
        .app_handle()
        .set_activation_policy(tauri::ActivationPolicy::Regular)
    {
        log::warn!("could not switch to the Regular activation policy: {error}");
    }
    if let Err(error) = window
        .show()
        .and_then(|_| window.unminimize())
        .and_then(|_| window.set_focus())
    {
        log::warn!("could not show the main window: {error}");
    }
}

pub fn keep_running(code: Option<i32>) -> bool {
    // Last-window closure has no exit code. Quit and updater restart do.
    code.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_window() -> Lifecycle {
        Lifecycle {
            phase: Phase::Open,
            ..Default::default()
        }
    }

    #[test]
    fn rapid_open_requests_create_one_window() {
        let mut state = Lifecycle::default();
        assert!(state.request_open());
        assert!(!state.request_open());
        assert_eq!(state.phase, Phase::Creating);
        assert!(state.finish_creation(state.generation));
        assert!(!state.finish_creation(state.generation));
    }

    #[test]
    fn stale_creation_completion_cannot_show_after_teardown_or_shutdown() {
        let mut state = Lifecycle::default();
        assert!(state.request_open());
        let stale = state.generation;
        state.destroyed();
        assert!(!state.finish_creation(stale));
        assert_eq!(state.phase, Phase::Absent);
        assert!(state.request_open());
        assert!(!state.finish_creation(stale));
        state.shutting_down = true;
        assert!(!state.finish_creation(state.generation));
    }

    #[test]
    fn open_during_destruction_waits_until_destroyed() {
        let mut state = open_window();
        assert!(state.request_close());
        assert!(!state.request_open());
        assert!(state.destroyed());
        assert!(state.request_open());
        assert!(!state.request_open());
    }

    #[test]
    fn operation_completion_closes_only_after_success() {
        for success in [false, true] {
            let mut state = open_window();
            let operation = state.begin_operation().unwrap();
            assert!(!state.request_close());
            assert_eq!(state.phase, Phase::Open);
            assert!(state.begin_operation().is_err());
            assert_eq!(state.end_operation(operation, success), success);
            assert!(!state.pending_close);
            assert_eq!(
                state.phase,
                if success {
                    Phase::Destroying
                } else {
                    Phase::Open
                }
            );
        }
    }

    #[test]
    fn stale_operation_cannot_close_a_replacement_window() {
        let mut state = open_window();
        let old = state.begin_operation().unwrap();
        state.destroyed();
        state.phase = Phase::Open;
        let current = state.begin_operation().unwrap();
        assert!(!state.request_close());
        assert!(!state.end_operation(old, true));
        assert_eq!(state.operation, Some(current));
        assert!(state.end_operation(current, true));
    }

    #[test]
    fn destruction_failure_only_restores_the_window_that_requested_it() {
        let mut state = Lifecycle::default();
        assert!(state.request_open());
        assert!(state.finish_creation(state.generation));
        let old = state.generation;
        assert!(state.request_close());
        state.destroy_failed(old);
        assert_eq!(state.phase, Phase::Open);
        assert!(state.request_close());
        state.destroyed();
        state.destroy_failed(old);
        assert_eq!(state.phase, Phase::Absent);
        assert!(state.request_open());
        state.destroy_failed(old);
        assert_eq!(state.phase, Phase::Creating);
        assert!(state.finish_creation(state.generation));
        assert!(state.request_close());
        state.destroy_failed(old);
        assert_eq!(state.phase, Phase::Destroying);
    }

    #[test]
    fn reopening_cancels_a_pending_close() {
        let mut state = open_window();
        let operation = state.begin_operation().unwrap();
        assert!(!state.request_close());
        assert!(state.request_open());
        assert!(!state.end_operation(operation, true));
        assert_eq!(state.phase, Phase::Open);
    }

    #[test]
    fn creation_failure_can_be_retried_and_shutdown_prevents_reopening() {
        let mut state = Lifecycle::default();
        assert!(state.request_open());
        state.phase = Phase::Absent;
        assert!(state.request_open());
        state.shutting_down = true;
        assert!(!state.request_open());
        state.reopen = true;
        assert!(!state.destroyed());
        assert!(state.begin_operation().is_err());
    }

    #[test]
    fn only_window_driven_exit_requests_are_prevented() {
        assert!(keep_running(None));
        assert!(!keep_running(Some(0)));
        assert!(!keep_running(Some(tauri::RESTART_EXIT_CODE)));
    }
}
