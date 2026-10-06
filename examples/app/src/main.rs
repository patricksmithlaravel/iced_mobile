//! The executable on the desktop, the web and iOS. Android loads the library
//! instead and starts the app through `iced::android_main!` in `src/lib.rs`.
// No console window behind a Windows release build.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> iced::Result {
    app::run() // never returns on iOS: UIKit keeps the main thread
}
