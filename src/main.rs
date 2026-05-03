#![cfg_attr(windows, windows_subsystem = "windows")]

mod manifest;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

fn main() {
    #[cfg(target_os = "macos")]
    macos::run();

    #[cfg(target_os = "windows")]
    windows::run();
}
