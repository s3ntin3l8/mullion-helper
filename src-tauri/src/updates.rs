use serde::Serialize;
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use tokio::sync::{watch, Mutex as AsyncMutex};

const INITIAL_DELAY: Duration = Duration::from_secs(3);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct UpdateResult {
    pub available: bool,
    pub version: Option<String>,
}

#[derive(Default)]
struct CheckState {
    cached: UpdateResult,
    completed: u64,
    last_result: Option<Result<UpdateResult, String>>,
}

struct Inner {
    state: Mutex<CheckState>,
    checking: AsyncMutex<()>,
    shutdown: watch::Sender<bool>,
}

#[derive(Clone)]
pub struct Updates(Arc<Inner>);

impl Default for Updates {
    fn default() -> Self {
        Self(Arc::new(Inner {
            state: Mutex::new(CheckState::default()),
            checking: AsyncMutex::new(()),
            shutdown: watch::channel(false).0,
        }))
    }
}

impl Updates {
    pub fn status(&self) -> UpdateResult {
        self.0
            .state
            .lock()
            .expect("update mutex poisoned")
            .cached
            .clone()
    }

    pub fn launch(&self, app: &AppHandle) {
        let updates = self.clone();
        let app = app.clone();
        let shutdown = self.0.shutdown.subscribe();
        tauri::async_runtime::spawn(async move {
            run_schedule(
                || async {
                    if let Err(error) = updates.check(&app).await {
                        log::warn!("background update check failed: {error}");
                    }
                },
                shutdown,
            )
            .await;
        });
    }

    pub fn shutdown(&self) {
        self.0.shutdown.send_replace(true);
    }

    pub async fn check(&self, app: &AppHandle) -> Result<UpdateResult, String> {
        self.check_using(
            || async {
                let update = crate::updater_builder(app)
                    .timeout(Duration::from_secs(30))
                    .build()
                    .map_err(|error| error.to_string())?
                    .check()
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(UpdateResult {
                    available: update.is_some(),
                    version: update.map(|value| value.version),
                })
            },
            |status| {
                if let Err(error) = app.emit("update-status", status) {
                    log::warn!("could not emit update status: {error}");
                }
            },
        )
        .await
    }

    async fn check_using<F, Fut, E>(&self, check: F, emit: E) -> Result<UpdateResult, String>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<UpdateResult, String>>,
        E: FnOnce(&UpdateResult),
    {
        let completed = self
            .0
            .state
            .lock()
            .expect("update mutex poisoned")
            .completed;
        let _checking = self.0.checking.lock().await;
        {
            let state = self.0.state.lock().expect("update mutex poisoned");
            // A manual and scheduled check arriving together share the same
            // result (including errors), rather than queueing duplicate HTTP.
            if state.completed != completed {
                return state
                    .last_result
                    .clone()
                    .expect("completed check has a result");
            }
        }
        let result = check().await;
        {
            let mut state = self.0.state.lock().expect("update mutex poisoned");
            state.completed = state.completed.wrapping_add(1);
            state.last_result = Some(result.clone());
            if let Ok(status) = &result {
                state.cached = status.clone();
            }
        }
        if let Ok(status) = &result {
            emit(status);
        }
        result
    }
}

async fn run_schedule<F, Fut>(mut check: F, mut shutdown: watch::Receiver<bool>)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = ()>,
{
    if *shutdown.borrow() {
        return;
    }
    let mut delay = INITIAL_DELAY;
    loop {
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(delay) => {},
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            _ = check() => {},
        }
        delay = CHECK_INTERVAL;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn available() -> UpdateResult {
        UpdateResult {
            available: true,
            version: Some("9.0.0".into()),
        }
    }

    #[tokio::test(start_paused = true)]
    async fn checks_after_startup_and_daily_without_a_window_then_stops() {
        let (shutdown, receiver) = watch::channel(false);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let task = tokio::spawn(run_schedule(
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                async {}
            },
            receiver,
        ));
        tokio::task::yield_now().await;
        tokio::time::advance(INITIAL_DELAY - Duration::from_secs(1)).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_secs(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        tokio::time::advance(CHECK_INTERVAL).await;
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        shutdown.send(true).unwrap();
        task.await.unwrap();
        tokio::time::advance(CHECK_INTERVAL).await;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn shutdown_cancels_an_inflight_background_check() {
        let (shutdown, receiver) = watch::channel(false);
        let (started, running) = tokio::sync::oneshot::channel();
        let mut started = Some(started);
        let task = tokio::spawn(run_schedule(
            move || {
                started.take().unwrap().send(()).unwrap();
                std::future::pending()
            },
            receiver,
        ));
        running.await.unwrap();
        shutdown.send(true).unwrap();
        task.await.unwrap();
    }

    #[tokio::test]
    async fn caches_success_but_errors_do_not_clear_available_updates() {
        let updates = Updates::default();
        assert_eq!(updates.status(), UpdateResult::default());
        let mut emitted = None;
        assert_eq!(
            updates
                .check_using(
                    || async { Ok(available()) },
                    |status| emitted = Some(status.clone())
                )
                .await,
            Ok(available())
        );
        assert_eq!(emitted, Some(available()));
        assert_eq!(updates.status(), available());
        assert!(updates
            .check_using(
                || async { Err("offline".into()) },
                |_| panic!("errors must not emit")
            )
            .await
            .is_err());
        assert_eq!(updates.status(), available());
        updates
            .check_using(|| async { Ok(UpdateResult::default()) }, |_| {})
            .await
            .unwrap();
        assert_eq!(updates.status(), UpdateResult::default());
    }

    #[tokio::test]
    async fn simultaneous_checks_share_one_request() {
        let updates = Updates::default();
        let first = updates.clone();
        let (started, running) = tokio::sync::oneshot::channel();
        let (release, wait) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            first
                .check_using(
                    || async {
                        started.send(()).unwrap();
                        wait.await.unwrap();
                        Ok(available())
                    },
                    |_| {},
                )
                .await
        });
        running.await.unwrap();
        let second = updates.clone();
        let waiter = tokio::spawn(async move {
            second
                .check_using(
                    || async { panic!("must reuse the in-flight result") },
                    |_| {},
                )
                .await
        });
        tokio::task::yield_now().await;
        release.send(()).unwrap();
        assert_eq!(task.await.unwrap(), Ok(available()));
        assert_eq!(waiter.await.unwrap(), Ok(available()));
    }
}
