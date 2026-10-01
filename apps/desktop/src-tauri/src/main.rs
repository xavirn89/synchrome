// Prevents an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Native messaging: Chromium passes the calling extension's origin as an argument.
    if let Some(origin) = args
        .iter()
        .skip(1)
        .find(|a| a.starts_with("chrome-extension://"))
    {
        std::process::exit(synchrome_desktop_lib::run_native_host(origin));
    }
    // Development-only CLI for end-to-end validation (absent from release builds).
    #[cfg(debug_assertions)]
    if args.get(1).is_some_and(|a| a.starts_with("--dev-")) {
        std::process::exit(synchrome_desktop_lib::run_dev_cli(&args[1..]));
    }
    synchrome_desktop_lib::run();
}
