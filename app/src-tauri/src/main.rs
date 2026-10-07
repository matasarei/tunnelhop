// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    th_app_lib::run();
}

#[cfg(not(windows))]
fn main() {
    eprintln!("TunnelHop's app runs on Windows only.");
    std::process::exit(1);
}
