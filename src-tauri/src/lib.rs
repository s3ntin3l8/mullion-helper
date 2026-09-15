mod headless_process;
mod migration;
mod supervisor;
mod tray_status;

use serde::Serialize;
use supervisor::{BridgeStatus, Settings, Supervisor};
use tauri::{
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, RunEvent, WindowEvent,
};
use tauri_plugin_autostart::MacosLauncher;
use tauri_plugin_log::{RotationStrategy, Target, TargetKind};
use tauri_plugin_opener::OpenerExt;
use tauri_plugin_updater::UpdaterExt;

#[tauri::command]
fn bridge_status(supervisor: tauri::State<'_, Supervisor>) -> BridgeStatus {
    supervisor.status()
}

#[tauri::command]
fn get_settings(supervisor: tauri::State<'_, Supervisor>) -> Settings {
    supervisor.settings()
}

#[tauri::command]
fn save_settings(
    settings: Settings,
    supervisor: tauri::State<'_, Supervisor>,
) -> Result<Settings, String> {
    supervisor.save_settings(settings)
}

#[tauri::command]
fn start_bridge(supervisor: tauri::State<'_, Supervisor>) -> BridgeStatus {
    supervisor.start()
}

#[tauri::command]
fn pause_bridge(supervisor: tauri::State<'_, Supervisor>) -> BridgeStatus {
    supervisor.pause()
}

#[tauri::command]
fn pair_bridge(
    payload: String,
    supervisor: tauri::State<'_, Supervisor>,
) -> Result<BridgeStatus, String> {
    supervisor.pair(&payload)
}

#[tauri::command]
fn unpair_bridge(supervisor: tauri::State<'_, Supervisor>) -> Result<BridgeStatus, String> {
    supervisor.unpair()
}

#[tauri::command]
fn diagnostics_path(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .app_log_dir()
        // tauri_plugin_log's TargetKind::LogDir { file_name: None } (see the
        // plugin registration below) names the file after
        // `package_info().name` — which is `productName` from
        // tauri.conf.json when set (confirmed against tauri-codegen's
        // context.rs), not Cargo.toml's package name. This app sets
        // productName to "Mullion Helper", so the real file is
        // "Mullion Helper.log". A previous version of this command
        // hardcoded "mullion-helper.log" (the Cargo package name) on the
        // opposite, incorrect assumption — read the same value the plugin
        // actually used instead of a second literal that can drift again.
        .map(|dir| {
            dir.join(format!("{}.log", app.package_info().name))
                .display()
                .to_string()
        })
        .map_err(|error| error.to_string())
}

#[derive(Serialize)]
struct UpdateResult {
    available: bool,
    version: Option<String>,
}

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateResult, String> {
    let update = updater_builder(&app)
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| error.to_string())?;
    Ok(UpdateResult {
        available: update.is_some(),
        version: update.map(|value| value.version),
    })
}

#[tauri::command]
async fn install_update(app: tauri::AppHandle) -> Result<(), String> {
    let update = updater_builder(&app)
        .build()
        .map_err(|error| error.to_string())?
        .check()
        .await
        .map_err(|error| error.to_string())?;
    let Some(update) = update else { return Ok(()) };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|error| error.to_string())?;
    app.restart();
}

fn updater_builder(app: &tauri::AppHandle) -> tauri_plugin_updater::UpdaterBuilder {
    let builder = app.updater_builder();
    #[cfg(windows)]
    {
        let app = app.clone();
        return builder.on_before_exit(move || {
            if let Some(tray_status) = app.try_state::<tray_status::TrayStatus>() {
                tray_status.shutdown_for_update();
            }
            if let Some(supervisor) = app.try_state::<Supervisor>() {
                supervisor.shutdown_for_update();
            }
            app.cleanup_before_exit();
        });
    }
    #[cfg(not(windows))]
    builder
}

fn show_main(app: &tauri::AppHandle) {
    // Confirmed in the field (PR #45's follow-up): merely *leaving* the
    // policy at Accessory while calling window.set_focus() is not enough —
    // focusing a window still implicitly promotes the app's Dock presence
    // at the AppKit level, and that promotion does not revert on its own
    // once the window is later hidden, so the Dock icon gets stuck showing
    // indefinitely until the user manually removes it. This is a known
    // Tauri/AppKit interaction with no clean "stay Accessory but still let
    // the window focus normally" fix — see the community-documented
    // workaround at https://github.com/tauri-apps/tauri/discussions/10774.
    // Embrace it instead of fighting it: explicitly go Regular for the
    // period the window is actually visible (a transient Dock icon while
    // the window is open is expected, normal behavior for a menu-bar-style
    // app — the same thing 1Password and similar tray utilities do), and
    // explicitly revert to Accessory in the close handler below, rather
    // than relying on an implicit revert that has proven not to happen.
    if let Some(window) = app.get_webview_window("main") {
        // Inside the if-let, not before it: if there's no window to show
        // (not yet created, or mid-teardown), there is also no
        // CloseRequested event coming to revert this — flipping to Regular
        // unconditionally could leave the app stuck there with nothing to
        // undo it.
        #[cfg(target_os = "macos")]
        if let Err(error) = app.set_activation_policy(tauri::ActivationPolicy::Regular) {
            log::warn!("could not switch to the Regular activation policy: {error}");
        }
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
        // First in the chain: this is the app's only logging facility, and
        // registering it early means every plugin/setup step after this one
        // can log through it too.
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    Target::new(TargetKind::LogDir { file_name: None }),
                    #[cfg(debug_assertions)]
                    Target::new(TargetKind::Stdout),
                ])
                // The plugin's own defaults (40KB, KeepOne) are tuned for a
                // server-style app with steady, moderate log volume. This
                // app's dominant failure mode is the opposite: a bridge
                // worker stuck in a reconnect/crash loop can write dozens of
                // multi-line stack traces in quick succession (see
                // Supervisor::handle_stderr), which at the default cap can
                // rotate away the very first crash — the one a user actually
                // wants to attach to a report — within a couple of loop
                // iterations. 1MB x 6 files is a trivial disk budget for a
                // desktop app's data dir and gives a crash loop room to
                // breathe before anything gets evicted. Tracked: issue #36.
                .max_file_size(1_000_000)
                .rotation_strategy(RotationStrategy::KeepSome(5))
                .build(),
        )
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            show_main(app)
        }))
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--background"]),
        ))
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            bridge_status,
            get_settings,
            save_settings,
            start_bridge,
            pause_bridge,
            pair_bridge,
            unpair_bridge,
            diagnostics_path,
            check_for_updates,
            install_update
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                // Undo show_main()'s explicit Regular switch (see the
                // comment there) — this is the revert that plain Accessory
                // policy alone was observed not to perform on its own.
                #[cfg(target_os = "macos")]
                if let Err(error) = window
                    .app_handle()
                    .set_activation_policy(tauri::ActivationPolicy::Accessory)
                {
                    log::warn!("could not switch back to the Accessory activation policy: {error}");
                }
            }
        })
        .setup(|app| {
            // Belt-and-braces alongside `Info.plist`'s `LSUIElement` key:
            // that key is advisory metadata LaunchServices applies from its
            // own cached registration for the bundle, which is populated at
            // install/launch and does not necessarily refresh across an
            // in-place update (or if a stale registration for an older
            // install — e.g. an unejected release DMG — shadows the real
            // one). A runtime `NSApplication` activation-policy change has
            // no such caching layer: it always takes effect for the
            // process that's actually running. Keep both; don't delete
            // either as "redundant" — they cover different failure modes of
            // the same "no dock icon" requirement.
            #[cfg(target_os = "macos")]
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);

            let data_dir = app.path().app_data_dir()?;
            let pending_migration = migration::import_legacy_credential(&data_dir);
            let supervisor =
                Supervisor::new(app.handle().clone(), data_dir).map_err(std::io::Error::other)?;
            let should_start = supervisor.inspect().unwrap_or(false);
            let pending_migration = pending_migration.and_then(|migration| {
                if should_start {
                    Some(migration)
                } else {
                    migration.rollback();
                    None
                }
            });
            app.manage(migration::MigrationState(std::sync::Mutex::new(
                pending_migration,
            )));
            app.manage(supervisor.clone());
            let launched_in_background =
                std::env::args().any(|argument| argument == "--background");

            let initial_status = supervisor.status();
            let status_item =
                MenuItem::with_id(app, "status", "Status: Ready to pair", false, None::<&str>)?;
            let separator = PredefinedMenuItem::separator(app)?;
            let open_item =
                MenuItem::with_id(app, "open", "Open Mullion Helper", true, None::<&str>)?;
            let toggle_item =
                MenuItem::with_id(app, "toggle", "Pause / Resume Bridge", true, None::<&str>)?;
            let open_logs_item =
                MenuItem::with_id(app, "open_logs", "Open Logs", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status_item,
                    &separator,
                    &open_item,
                    &toggle_item,
                    &open_logs_item,
                    &quit_item,
                ],
            )?;
            let tray_status =
                tray_status::TrayStatus::new(app.handle().clone(), status_item, &initial_status);

            TrayIconBuilder::with_id(tray_status::TRAY_ID)
                .icon(tray_status.initial_icon())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .tooltip("Mullion Helper — Ready to pair")
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_main(tray.app_handle());
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "open" => show_main(app),
                    "toggle" => {
                        let supervisor = app.state::<Supervisor>();
                        match supervisor.status().state {
                            supervisor::BridgeState::Paused => {
                                supervisor.start();
                            }
                            // Nothing to pause/resume before pairing exists
                            // — falling through to pause() here would flip
                            // to Paused, which hides the onboarding panel
                            // (needsPairing in App.tsx only covers Unpaired
                            // and NeedsPairing) behind a Start button that
                            // just leads straight back to Unpaired anyway.
                            // Open the window so the user lands on the
                            // pairing form instead of that detour.
                            supervisor::BridgeState::Unpaired
                            | supervisor::BridgeState::NeedsPairing => {
                                show_main(app);
                            }
                            _ => {
                                supervisor.pause();
                            }
                        }
                    }
                    "open_logs" => {
                        if let Ok(log_dir) = app.path().app_log_dir() {
                            let _ = app
                                .opener()
                                .open_path(log_dir.display().to_string(), None::<&str>);
                        }
                    }
                    "quit" => {
                        app.state::<Supervisor>().shutdown();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            app.manage(tray_status.clone());
            tray_status.update(&initial_status);
            tray_status.launch();
            supervisor.launch();
            if should_start {
                supervisor.start();
            }

            if !launched_in_background {
                show_main(app.handle());
            }

            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building Mullion Helper");

    builder.run(|app, event| {
        if matches!(event, RunEvent::Exit | RunEvent::ExitRequested { .. }) {
            if let Some(tray_status) = app.try_state::<tray_status::TrayStatus>() {
                tray_status.shutdown();
            }
            if let Some(supervisor) = app.try_state::<Supervisor>() {
                supervisor.shutdown();
            }
        }
    });
}

#[cfg(test)]
mod installer_tests {
    use serde_json::Value;

    #[test]
    fn nsis_preinstall_hook_targets_the_bundled_worker() {
        let config: Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(
            config.pointer("/bundle/windows/nsis/installerHooks"),
            Some(&Value::String("nsis/installer-hooks.nsh".into()))
        );
        assert_eq!(
            config.pointer("/bundle/externalBin/0"),
            Some(&Value::String("binaries/mullion-bridge-worker".into()))
        );

        let hook = include_str!("../nsis/installer-hooks.nsh");
        let process_checks: Vec<_> = hook
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("!insertmacro CheckIfAppIsRunning"))
            .collect();
        assert_eq!(
            process_checks,
            [
                r#"!insertmacro CheckIfAppIsRunning "mullion-bridge-worker.exe" "Mullion Bridge Worker""#
            ]
        );
    }
}
