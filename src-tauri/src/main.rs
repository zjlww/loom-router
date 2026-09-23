// Keep the console for the headless CLI; only the optional desktop build is a
// Windows GUI application.
#![cfg_attr(
    all(feature = "desktop", not(debug_assertions)),
    windows_subsystem = "windows"
)]

fn main() {
    #[cfg(feature = "desktop")]
    if std::env::args_os().len() == 1 {
        loom_router_lib::run();
        return;
    }

    if let Err(error) = loom_router_lib::cli::run() {
        eprintln!("loom-router: {error:#}");
        std::process::exit(1);
    }
}
