// Prevents an extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // The screenshot decoder runs in a child process of the app; it exits from here.
    if ps5upload_desktop_lib::screenshot_decoder_child() {
        return;
    }
    ps5upload_desktop_lib::run();
}
