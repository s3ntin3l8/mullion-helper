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

#[derive(Serialize)]
struct UpdateResult {
    available: bool,
    version: Option<String>,
}

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> Result<UpdateResult, String> {
    let update = app
        .updater()
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
    let update = app
        .updater()
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

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let builder = tauri::Builder::default()
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
            check_for_updates,
            install_update
        ])
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .setup(|app| {
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
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &status_item,
                    &separator,
                    &open_item,
                    &toggle_item,
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
                        if matches!(supervisor.status().state, supervisor::BridgeState::Paused) {
                            supervisor.start();
                        } else {
                            supervisor.pause();
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
            supervisor.clone().launch();
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
            if let Some(supervisor) = app.try_state::<Supervisor>() {
                supervisor.shutdown();
            }
            if let Some(tray_status) = app.try_state::<tray_status::TrayStatus>() {
                tray_status.shutdown();
            }
        }
    });
}
