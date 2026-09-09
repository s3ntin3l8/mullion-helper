// Prevents an additional console window on Windows in release builds. Kept
// even though this app is a background tray process with no window of its
// own — a stray console flash on launch is the kind of thing that reads as
// broken on first run, cheap to prevent unconditionally.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    mullion_helper_lib::run();
}
